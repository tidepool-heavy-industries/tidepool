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

// Fixture view changes reopen one complete issued packet through production
// recovery; rows and native decisions cannot be supplied independently.
fn restore_fixture_support_in_view(
    issued: &ExactDeclarationContext,
    view: ArtifactView,
) -> Result<crate::artifact_inventory::CompilerSourceSupport, CompileError> {
    issued
        .compiler_source_support()
        .native_availability()
        .validate_role_coverage(issued.compiler_input_projection())?;
    let recovered = recover_fixture_compiler_support(
        issued.compiler_input_projection().clone(),
        view.retain_custody(issued.artifact_view())?,
        [],
    )?;
    recovered.restore_snapshot_evidence(&issued.snapshot())
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
fn wire_classification(request: &ExactCompilationRequest, module: &str) -> Value {
    scope(request)[6]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.as_array().unwrap()[1] == text(module))
        .unwrap()
        .as_array()
        .unwrap()[9]
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

// The eager reference uses a genuine issuer view and independently withdraws
// executable demand. It never reconstructs a view from the opaque offer owner.
fn eager_artifact_custody_reference(
    issuer: &ArtifactView,
    offer: &crate::artifact_inventory::IssuedNativeOffer,
) -> ArtifactView {
    let mut graph = offer.custody_snapshot().graph_selection().unwrap();
    graph.native_groups.clear();
    graph.roots.clear();
    graph.namespace.clear();
    graph.custody_roots = graph
        .bindings
        .iter()
        .copied()
        .map(crate::artifact_inventory::ArtifactBindingNode::Artifact)
        .collect();
    issuer
        .select_issued(
            vec![],
            &crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(
                graph,
            ),
        )
        .unwrap()
}

fn ordered_offer_retention(
    context: &ArtifactView,
    eager_custody: &[ArtifactView],
) -> Result<ArtifactView, CompileError> {
    eager_custody
        .iter()
        .try_fold(context.clone(), |view, custody| {
            view.merge_fixture_selections(custody)
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
    eager_custody: &[ArtifactView],
) {
    assert_eq!(offers.len(), eager_custody.len());
    for end in 0..=offers.len() {
        let prefix = &offers[..end];
        assert_retention_result_matches(
            ordered_offer_retention(context, &eager_custody[..end]),
            retain_native_offers(context, prefix),
        );
        assert_retention_result_matches(
            ordered_offer_retention(context, &eager_custody[..end]),
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
    let mut eager_custody = Vec::new();
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
        for offer in [&full, &partial, &empty] {
            eager_custody.push(eager_artifact_custody_reference(&source, offer));
        }
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
    let selected_custody = indices
        .iter()
        .map(|index| eager_custody[*index % eager_custody.len()].clone())
        .collect::<Vec<_>>();
    assert_offer_retention_matches_ordered_fold(
        &context,
        &projection,
        &selected,
        &selected_custody,
    );
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
    let eager_custody = offers
        .iter()
        .map(|offer| eager_artifact_custody_reference(&source, offer))
        .collect::<Vec<_>>();
    // Warm only immutable inputs. The cumulative result views have not been read.
    source.entries();
    for custody in &eager_custody {
        custody.entries();
    }
    let mut current = source.clone();
    let before = inventory.metrics();
    for (index, custody) in eager_custody.iter().enumerate() {
        let previous = inventory.metrics();
        current = current.merge_fixture_selections(custody).unwrap();
        let merged = inventory.metrics();
        current.entries();
        let observed = inventory.metrics();
        eprintln!(
            "ordered_prefix={index} merge_graph_visits={} merge_entry_copies={} owner_lookups={} result_graph_visits={} result_entry_copies={}",
            merged.graph_visits - previous.graph_visits,
            merged.entry_handle_copies - previous.entry_handle_copies,
            merged.admission_owner_lookups - previous.admission_owner_lookups,
            observed.graph_visits - merged.graph_visits,
            observed.entry_handle_copies - merged.entry_handle_copies
        );
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
    let expected = ordered_offer_retention(&source, &eager_custody).unwrap();
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
    eprintln!(
        "retention_work_comparison ordered_graph_visits={old_visits} batch_graph_visits={batch_visits} ordered_owner_lookups={old_lookups} batch_owner_lookups={batch_lookups} ordered_entry_copies={} batch_entry_copies={}",
        old.entry_handle_copies - before.entry_handle_copies,
        batched.entry_handle_copies - before_batch.entry_handle_copies
    );
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
            let expected = offer.custody_snapshot().graph_selection().unwrap();
            assert!(expected
                .bindings
                .iter()
                .all(|node| actual.bindings.contains(node)));
            let retained_offer = retained
                .native_offers()
                .iter()
                .find(|retained| retained.snapshot_facts() == offer.snapshot_facts())
                .expect("retained availability preserves this exact issuer and mode");
            assert_eq!(
                retained_offer
                    .custody_snapshot()
                    .binding_dependencies()
                    .unwrap(),
                offer.custody_snapshot().binding_dependencies().unwrap()
            );
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
    let mut offers = Vec::new();
    let mut eager_custody = Vec::new();
    let mut completed = Vec::new();
    for _ in 0..4 {
        let issuer = source
            .select_issued(
                vec![],
                &crate::artifact_inventory::ExactArtifactSelection::capture(&source),
            )
            .unwrap();
        let metadata = projection
            .project_metadata(issuer.metadata_snapshot())
            .unwrap();
        let owner = issuer
            .retain_materialization(&metadata, |parents| {
                context.materialize_retained_artifacts(
                    &metadata,
                    parents,
                    tempfile::tempdir().unwrap(),
                )
            })
            .unwrap();
        let offer =
            IssuedNativeOffer::issue(entry.descriptor.id, &issuer, Some(proof.clone())).unwrap();
        eager_custody.push(eager_artifact_custody_reference(&issuer, &offer));
        offers.push(offer);
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
        let selected_custody = order
            .iter()
            .map(|index| eager_custody[*index].clone())
            .collect::<Vec<_>>();
        let expected = ordered_offer_retention(&source, &selected_custody).unwrap();
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
    let private = OriginalCompilerInputs::from_native_availability(
        &baseline,
        producer,
        std::slice::from_ref(&consumer),
    )
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
    assert_eq!(
        private.native_availability.rows().map(|row| row.original.descriptor.id).collect::<BTreeSet<_>>(),
        BTreeSet::from([private.native_offers[0].original()]),
        "normalizing a fresh fragment against its effective namespace must not grant baseline bodies"
    );
    assert!(private
        .native_availability
        .rows()
        .all(|row| row.original.descriptor.owner != dependency));
    assert_eq!(private.native_offers.len(), 1);
    let groups = private.native_offers[0]
        .issued_group_carriers()
        .unwrap()
        .into_keys()
        .collect::<BTreeSet<_>>();
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
        .projection()
        .validate(&final_inputs.artifacts())
        .unwrap();
    assert_eq!(
        final_inputs
            .projection()
            .snapshot_roles()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([dependency.clone(), offered.clone()])
    );
    assert!(final_inputs.projection().snapshot_roles()[&dependency]
        .original()
        .is_none());
    assert_eq!(
        final_inputs.artifacts().selected_native_groups(),
        roots,
        "private availability cannot add executable body roots"
    );
    let final_proof = final_inputs
        .native_original_requirements()
        .into_iter()
        .find(|proof| proof.original == original)
        .unwrap();
    assert_eq!(final_proof.original, original);
    let availability = final_inputs.native_availability(original).unwrap();
    assert_eq!(availability.original_owner(), consumer.owner());
    assert_eq!(availability.mode, NativeTransportMode::Full);
    let support = availability
        .dependency_support(
            NativeGroupKey {
                artifact: original,
                original_ordinal: 7,
            },
            consumer.owner(),
        )
        .expect("the final request carries this exact issued group");
    assert_eq!(support.groups().collect::<BTreeSet<_>>(), groups);
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
    let dependency = |proof: &NativeOriginalRequirement, view: &ArtifactView, stage| {
        proof
            .issued_groups(view)
            .unwrap_or_else(|error| panic!("{stage}: {error}"))
            .into_iter()
            .find(|group| {
                group.binding.artifact() == native_entry.descriptor.id
                    && group.original_ordinal == 7
            })
            .unwrap()
    };
    let left_group = dependency(&left_proof, &left, "left issuer");
    let right_group = dependency(&right_proof, &right, "right issuer");
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
    assert_eq!(
        dependency(retained.proof().unwrap(), &view, "merged issuer custody"),
        left_group
    );
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
        let selected_graph = selected.capture_graph_selection();
        let mismatch = selected.retain_custody(&left).unwrap();
        let mismatch_graph = mismatch.capture_graph_selection();
        assert_eq!(mismatch_graph.namespace, selected_graph.namespace);
        assert_eq!(mismatch_graph.roots, selected_graph.roots);
        assert_eq!(
            left_proof.issued_groups(&mismatch).unwrap(),
            left_proof.issued_groups(&left).unwrap(),
            "cross-inventory custody preserves the actual historical group identities"
        );
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
    let close_source = |selected: &ArtifactView, supplied: &[NativeOriginalRequirement]| {
        let offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
            &baseline, selected, supplied,
        )
        .unwrap();
        let support = recover_fixture_compiler_support(
            baseline.clone(),
            selected.retain_custody(&broad).unwrap(),
            offers,
        )
        .unwrap();
        let source_context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: support,
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        });
        ProtectedScaffoldRequirements::default().compiler_inputs(&source_context, None)
    };
    let scoped_execution_roots = |view: &ArtifactView| {
        view.capture_graph_selection()
            .roots
            .into_iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(group),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
    };
    let opaque_dependency = |offer: &crate::artifact_inventory::IssuedNativeOffer,
                             proof: &NativeOriginalRequirement| {
        assert_eq!(offer.original(), consumer_entry.descriptor.id);
        assert!(offer.is_full());
        assert_eq!(offer.proof(), Some(proof));
        let groups = offer.issued_group_carriers().unwrap();
        let dependency = *groups
            .keys()
            .find(|group| {
                group.binding.artifact() == native_entry.descriptor.id
                    && group.original_ordinal == 7
            })
            .unwrap();
        let consumer = *groups
            .keys()
            .find(|group| {
                group.binding.artifact() == consumer_entry.descriptor.id
                    && group.original_ordinal == 7
            })
            .unwrap();
        assert!(offer
            .custody_snapshot()
            .binding_dependencies()
            .unwrap()
            .contains(&(
                ArtifactBindingNode::Group(consumer),
                ArtifactBindingNode::Group(dependency),
                crate::artifact_inventory::ArtifactDependency::NativeGroup {
                    dependent_ordinal: 7,
                    required_ordinal: 7,
                },
            )));
        dependency
    };
    for (index, supplied, expected) in [
        (0, vec![left_proof.clone()], left_group),
        (1, vec![left_proof.clone(), left_proof.clone()], left_group),
        (2, Vec::new(), right_group),
    ] {
        let source =
            close_source(if supplied.is_empty() { &right } else { &left }, &supplied).unwrap();
        let expected_roots =
            scoped_execution_roots(if supplied.is_empty() { &right } else { &left });
        assert_eq!(scoped_execution_roots(source.artifacts()), expected_roots);
        let input = OriginalCompilerInputs::from_checked_support_products(
            source.compiler_source_support(),
            producer.sha256(),
            std::slice::from_ref(consumer),
        )
        .unwrap();
        assert_eq!(
            scoped_execution_roots(input.issued_artifacts()),
            expected_roots,
            "private complete issuance preserves the caller's exact chosen execution roots"
        );
        let issued = input
            .native_original_requirements()
            .iter()
            .find(|proof| proof.original == consumer_entry.descriptor.id)
            .cloned()
            .unwrap();
        assert!(
            issued.issued_groups(input.issued_artifacts()).is_err(),
            "carrier-only custody cannot authenticate the opaque consumer Group proof"
        );
        assert!(!input
            .issued_artifacts()
            .selected_native_groups()
            .iter()
            .any(|key| key.artifact == consumer_entry.descriptor.id));
        let consumer_offer = input
            .native_offers()
            .iter()
            .find(|offer| offer.original() == consumer_entry.descriptor.id)
            .unwrap()
            .clone();
        assert_eq!(
            opaque_dependency(&consumer_offer, &issued),
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
        let retained_offer = request
            .compiler_inputs()
            .compiler_source_support()
            .native_offers()
            .iter()
            .find(|offer| offer.original() == consumer_entry.descriptor.id)
            .unwrap();
        assert!(retained_offer.same_issuance(&consumer_offer));
        assert_eq!(opaque_dependency(retained_offer, &retained), expected);
        assert_eq!(wire_mode(&request, "PrivateConsumer"), full_mode());
        assert_eq!(wire_mode(&request, "CarriedDependency"), issued_mode(&[7]));
    }
    let left_source = close_source(&left, &[left_proof]).unwrap();
    let right_source = close_source(&right, &[right_proof]).unwrap();
    assert!(matches!(
        left_source.compiler_source_support().merge(right_source.compiler_source_support()),
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
                    assert_eq!(
                        input.is_ok(),
                        expected,
                        "demanded={demanded}, selected={selected:?}, private={explicit_required}, extra={unrelated}"
                    );
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
                                .owner
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
    let transport_metadata = projection
        .project_metadata(base.metadata_snapshot())
        .unwrap();
    let transport = NativeCompilerTransport::issue_checked(
        &projection,
        &transport_metadata,
        &full,
        &[
            ancestor_offer.clone(),
            dependency_offer.clone(),
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
        .any(|offer| offer.original() == entries[0].descriptor.id));
    assert!(receipt
        .offers()
        .all(|offer| offer.original() != entries[2].descriptor.id));
    assert!(receipt.permits(7));
    assert!(!receipt.permits(9001));
    assert!(matches!(receipt.mode, NativeTransportMode::IssuedGroups));
    let unrelated_payload =
        Arc::downgrade(&full.metadata_snapshot().artifacts[&entries[2].descriptor.id]);
    let b_id = entries[1].descriptor.id;
    let a_id = entries[0].descriptor.id;
    let demand = source_import(&recovered[1].product, 7);
    let denied_demand = source_import(&recovered[1].product, 9001);
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
    let b_context = {
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: recover_fixture_compiler_support(
                b_projection.clone(),
                b.clone(),
                [ancestor_offer.clone(), dependency_offer.clone()],
            )
            .unwrap(),
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        });
        let missing_row = ProtectedScaffoldRequirements::default()
            .compiler_inputs(&context, None)
            .err()
            .expect("a selected original without its issued permission row must refuse");
        assert!(
            matches!(&missing_row, CompileError::ExtractFailed(detail)
                if detail == "exact declaration context: selected original lacks its normalized native permission row"),
            "unexpected missing-row refusal: {missing_row:?}",
        );
        context.rebuild(|facts| {
            facts.compiler_support = facts
                .compiler_support
                .retain_normalized_availability(
                    &RequestNativeAvailability::from_issued_rows([Arc::clone(&receipt)]).unwrap(),
                )
                .expect("the already issued B row matches its selected canonical role");
        })
    };
    let inputs = ProtectedScaffoldRequirements::default()
        .compiler_inputs(&b_context, None)
        .unwrap();
    let owned_receipt = inputs.native_availability(b_id).unwrap();
    assert_eq!(owned_receipt.offers().len(), 2);
    assert!(owned_receipt.permits(7));
    assert!(!owned_receipt.permits(9001));
    let key = NativeGroupKey {
        artifact: b_id,
        original_ordinal: 7,
    };
    let exact = owned_receipt
        .dependency_support(key, recovered[1].product.owner())
        .unwrap()
        .groups()
        .collect::<BTreeSet<_>>();
    assert!(!exact.is_empty());
    assert!(exact
        .iter()
        .all(|group| group.binding.artifact() == b_id && group.original_ordinal == 7));
    assert!(owned_receipt
        .dependency_support(
            NativeGroupKey {
                artifact: b_id,
                original_ordinal: 9001
            },
            recovered[1].product.owner()
        )
        .is_none());
    assert!(owned_receipt
        .dependency_support(
            NativeGroupKey {
                artifact: a_id,
                original_ordinal: 7
            },
            recovered[0].product.owner()
        )
        .is_none());
    assert!(owned_receipt
        .dependency_support(key, recovered[0].product.owner())
        .is_none());
    drop(owned_receipt);
    let validation = PackageInterfaceValidation::default();
    let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        &inputs,
        &inputs.metadata,
        &validation.inventory,
    )
    .unwrap();
    let namespace = inputs
        .projection()
        .roles()
        .into_iter()
        .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
        .collect::<BTreeSet<_>>();
    let metadata = inputs.artifacts().metadata_snapshot();
    let native_owners = metadata
        .artifacts
        .iter()
        .filter_map(|(id, entry)| match &entry.payload {
            ArtifactPayload::Original(product) => Some((
                crate::artifact_inventory::NativeOwnerKey::from_owner(product.owner()),
                *id,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        selected
            .artifact_ids_from_entries(&metadata.artifacts)
            .unwrap(),
        namespace
    );
    assert_eq!(
        selected
            .artifact_ids_from_bound_entries(
                &metadata.artifacts,
                &native_owners,
                &inputs.artifacts(),
                &BTreeSet::new()
            )
            .unwrap(),
        namespace
    );
    {
        let continuation =
            ContinuationNativeInputs::from_selection(&selected, inputs.projection()).unwrap();
        let catalog =
            crate::artifact_inventory::NativeDependencyCatalog::from_request(&continuation);
        let a_interface = Arc::new(ArtifactEntry::canonical(
            recovered[0].product.module_interface().unwrap().clone(),
        ));
        let receiver = full
            .select_roots(vec![a_interface.descriptor.id, b_id])
            .unwrap();
        let effective =
            CompilerInputProjection::from_issued_entries(&[a_interface, entries[1].clone()])
                .unwrap();
        let selected_groups = receiver.selected_native_groups();
        assert!(selected_groups
            .iter()
            .any(|key| key.artifact == b_id && key.original_ordinal == 7));
        assert!(
            selected_groups.iter().all(|key| key.artifact == b_id),
            "the actual B original selects its own groups; A stays canonical-only"
        );
        assert!(
            ancestor_offer
                .qualified_private_availability(&effective, &receiver)
                .is_some(),
            "the retained Full would qualify without the actual physical row restriction"
        );
        let advertised = BTreeSet::from([NativeGroupKey {
            artifact: a_id,
            original_ordinal: 7,
        }]);
        assert!(
            catalog
                .qualified_availability(
                    &effective,
                    &receiver,
                    &advertised,
                    std::slice::from_ref(&ancestor_offer)
                )
                .unwrap()
                .is_empty(),
            "B's IssuedGroups row must not turn its retained Full A proof into A permission"
        );
    }
    drop(inputs);
    drop(b_context);
    drop(dependency_offer);
    drop(transport);
    drop(transport_metadata);
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
    let continuation =
        ContinuationNativeInputs::from_selection(&selected, &private.projection).unwrap();
    let catalog = crate::artifact_inventory::NativeDependencyCatalog::from_request(&continuation);
    let admitted = private
        .projection
        .admit_interfaces_with_native_offers(
            &private.artifacts,
            vec![],
            &private.native_offers,
            &catalog,
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
    assert!(matches!(
        admitted.native_requirements_from_roots(&[crate::artifact_inventory::NativeRequirementRoot::Group { artifact: b_id, original_ordinal: 7 }]),
        Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                artifact, original_ordinal: 7,
            } if artifact == b_id)
    ));
    let consume = |view: &ArtifactView,
                   selection: &crate::certified_products::CertifiedSourceSelection,
                   imports: &[crate::certified_products::PendingImportOwner]| {
        let mut fixed = view
            .inventory()
            .bind_segment(view, vec![], selection)
            .expect("bind the actual receipt owner before body demand");
        let demanded = view.inventory().admit_bound_target(
            &mut fixed,
            view,
            NativeArtifactDemand::CertifiedTargetImports(imports),
            vec![],
            selection,
        )?;
        drop(fixed);
        demanded
            .clone()
            .bind_target(None, (imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
    };
    let mut failed_then_retried = admitted
        .inventory()
        .bind_segment(&admitted, vec![], &selected)
        .unwrap();
    let fixed_before = failed_then_retried.observed_binding_state();
    let cursor_before = admitted.capture_graph_selection();
    let mixed = [demand.clone(), denied_demand.clone()];
    assert!(matches!(admitted.inventory().admit_bound_target(
        &mut failed_then_retried, &admitted,
        NativeArtifactDemand::CertifiedTargetImports(&mixed), vec![], &selected,
    ), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
        crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
            artifact, original_ordinal: 9001,
        } if artifact == b_id)));
    assert_eq!(
        failed_then_retried.observed_binding_state(),
        fixed_before,
        "a valid staged B7 followed by denied B9001 publishes no payload or choice"
    );
    assert_eq!(admitted.capture_graph_selection(), cursor_before);
    let retried = admitted
        .inventory()
        .admit_bound_target(
            &mut failed_then_retried,
            &admitted,
            NativeArtifactDemand::CertifiedTargetImports(std::slice::from_ref(&demand)),
            vec![],
            &selected,
        )
        .unwrap();
    drop(failed_then_retried);
    assert_eq!(
        retried
            .clone()
            .bind_target(None, (std::slice::from_ref(&demand)).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap(),
        exact
    );
    let groups = consume(&admitted, &selected, std::slice::from_ref(&demand))
        .expect("the issued B receipt authorizes its exact group7");
    assert_eq!(groups, exact);
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

    assert!(matches!(
        consume(&admitted, &selected, std::slice::from_ref(&denied_demand)),
        Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                artifact, original_ordinal: 9001,
            } if artifact == b_id)
    ));

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
        .flat_map(|availability| availability.offers().cloned())
        .collect::<Vec<_>>();
    let catalog = crate::artifact_inventory::NativeDependencyCatalog::from_source(
        request.compiler_source_support(),
    )
    .unwrap();
    let request_selection =
        crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            &request,
            &request.metadata,
            &validation.inventory,
        )
        .unwrap();
    let selected_view = request
        .projection()
        .admit_interfaces_with_native_offers(&request.artifacts(), vec![], &offers, &catalog)
        .unwrap();
    let graph = selected_view.capture_graph_selection();
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
    let requested_groups = consume(
        &selected_view,
        &request_selection,
        std::slice::from_ref(&demand),
    )
    .unwrap();
    assert_eq!(requested_groups, exact);
    assert_eq!(
        requested_groups
            .iter()
            .map(|group| (group.binding.artifact(), group.original_ordinal))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(b_id, 7)])
    );
    assert!(!request.native_transport.availability[&b_id].permits(9001));
    assert!(matches!(
        consume(&selected_view, &request_selection, &[denied_demand]),
        Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                artifact, original_ordinal: 9001,
            } if artifact == b_id)
    ));

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
            .all(|offer| offer.proof().is_none() && !offer.is_full()));
        assert!(
            matches!(baseline.artifacts().native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: entry.descriptor.id, original_ordinal: 7,
        }]), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == entry.descriptor.id)),
            "a real demand without an exact choice must still refuse"
        );
        for chosen in [&left, &right] {
            let private = OriginalCompilerInputs {
                native_availability: RequestNativeAvailability::issue_checked(
                    &projection,
                    &retain_native_offers(&base, std::slice::from_ref(chosen)).unwrap(),
                    std::slice::from_ref(chosen),
                )
                .unwrap(),
                demand: crate::declaration_context::OriginalInputDemand::RolePolicy,
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
                    .issued_group_carriers(request.compiler_inputs().artifacts())
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
                    context.owner.facts.producer,
                    &binder.unit,
                    &binder.module,
                    BTreeMap::new(),
                ),
            ))
        })
        .collect::<Vec<_>>();
    let projection = context
        .owner
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
                .artifacts()
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
        native_availability: RequestNativeAvailability::issue_checked(
            &projection,
            &retain_native_offers(&b, &offers).unwrap(),
            &offers,
        )
        .unwrap(),
        demand: crate::declaration_context::OriginalInputDemand::RolePolicy,
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
    let issued = ProtectedScaffoldRequirements::default()
        .compiler_inputs(&context(b.clone(), projection.clone()), Some(&private))
        .expect("without a conflicting selected A, B retains its real A9001 dependency");
    let catalog = crate::artifact_inventory::NativeDependencyCatalog::from_source(
        issued.compiler_source_support(),
    )
    .unwrap();
    projection
        .admit_interfaces_with_native_offers(
            &private.artifacts,
            vec![],
            &private.native_offers,
            &catalog,
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
    let typed = selected_projection
        .admit_interfaces_with_native_offers(
            &private.artifacts,
            vec![replacement],
            &private.native_offers,
            &catalog,
        )
        .expect("type-only custody does not consume the incompatible native backedge");
    assert!(typed.selected_native_groups().is_empty());
    selected_projection.validate(&typed).unwrap();
    let group = |artifact, ordinal| {
        *support
            .keys()
            .find(|group| group.binding.artifact() == artifact && group.original_ordinal == ordinal)
            .unwrap()
    };
    assert!(
        typed.binding_dependencies().contains(&(
            ArtifactBindingNode::Group(group(b_id, 7)),
            ArtifactBindingNode::Group(group(a_id, 9001)),
            crate::artifact_inventory::ArtifactDependency::NativeGroup {
                dependent_ordinal: 7,
                required_ordinal: 9001,
            },
        )),
        "type-only custody preserves the exact backedge that native consumption refuses"
    );
}

#[test]
fn dependency_support_keeps_consumed_historical_group_instead_of_parent_root() {
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement,
    };
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let b_seed = product("ScopedBackedgeB", 172);
    let a_seed = dependent_product(
        "ScopedBackedgeA",
        171,
        vec![(7, vec![source_import(&b_seed, 7)]), (9001, vec![])],
    );
    let b = dependent_product(
        "ScopedBackedgeB",
        172,
        vec![(7, vec![source_import(&a_seed, 9001)]), (9001, vec![])],
    );
    let a = dependent_product(
        "ScopedBackedgeA",
        171,
        vec![(7, vec![source_import(&b, 7)]), (9001, vec![])],
    );
    assert_eq!(
        a_seed.owner(),
        a.owner(),
        "the historical backedge names the same immutable source owner"
    );
    let recovered = recovered_witness_fixtures(&[a, b]);
    let entries = recovered
        .iter()
        .map(|row| {
            Arc::new(ArtifactEntry::original(producer.sha256(), row.product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    let (a_id, b_id) = (entries[0].descriptor.id, entries[1].descriptor.id);
    let inventory = ArtifactInventory::default();
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let marker = |module, version| {
        Arc::new(ArtifactEntry::canonical(
            product(module, version).module_interface().unwrap().clone(),
        ))
    };
    let historical = inventory
        .admit_recovery_selection(
            &base,
            vec![entries[0].clone(), marker("ScopedHistoricalMarker", 173)],
            &BTreeSet::from([NativeGroupKey {
                artifact: a_id,
                original_ordinal: 9001,
            }]),
        )
        .unwrap();
    let historical_group = historical
        .capture_graph_selection()
        .native_groups
        .into_iter()
        .find(|group| group.binding.artifact() == a_id && group.original_ordinal == 9001)
        .unwrap();
    let current = inventory
        .admit_recovery_selection(
            &base,
            vec![
                entries[0].clone(),
                entries[1].clone(),
                marker("ScopedCurrentMarker", 174),
            ],
            &BTreeSet::from([
                NativeGroupKey {
                    artifact: a_id,
                    original_ordinal: 7,
                },
                NativeGroupKey {
                    artifact: a_id,
                    original_ordinal: 9001,
                },
                NativeGroupKey {
                    artifact: b_id,
                    original_ordinal: 7,
                },
            ]),
        )
        .unwrap();
    let current_group = current
        .capture_graph_selection()
        .native_groups
        .into_iter()
        .find(|group| group.binding.artifact() == a_id && group.original_ordinal == 9001)
        .unwrap();
    let carrier = |view: &ArtifactView| {
        view.capture_graph_selection()
            .namespace
            .into_iter()
            .find(|binding| binding.artifact == a_id)
            .unwrap()
    };
    assert_eq!(carrier(&historical), carrier(&current));
    let group_carrier = |view: &ArtifactView, group| {
        view.binding_dependencies()
            .into_iter()
            .filter_map(|(from, to, label)| {
                if from != ArtifactBindingNode::Group(group)
                    || label != crate::artifact_inventory::ArtifactDependency::Interface
                {
                    return None;
                }
                match to {
                    ArtifactBindingNode::Artifact(binding) if binding.artifact == a_id => {
                        Some(binding)
                    }
                    _ => None,
                }
            })
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        group_carrier(&historical, historical_group),
        BTreeSet::from([carrier(&historical)])
    );
    assert_eq!(
        group_carrier(&current, current_group),
        BTreeSet::from([carrier(&historical)])
    );
    assert_ne!(
        historical_group, current_group,
        "independent marker scopes issue distinct groups on the same carrier"
    );
    let b_group = current
        .capture_graph_selection()
        .native_groups
        .into_iter()
        .find(|group| group.binding.artifact() == b_id && group.original_ordinal == 7)
        .unwrap();
    let expected_edge = (
        ArtifactBindingNode::Group(b_group),
        ArtifactBindingNode::Group(current_group),
        crate::artifact_inventory::ArtifactDependency::NativeGroup {
            dependent_ordinal: 7,
            required_ordinal: 9001,
        },
    );
    assert!(current.binding_dependencies().contains(&expected_edge));
    let current = current.merge_fixture_selections(&historical).unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(a_id, &current)
        .unwrap()
        .unwrap();
    let offer = IssuedNativeOffer::issue(a_id, &current, Some(proof)).unwrap();
    assert!(
        !offer.is_full(),
        "competing same-carrier groups cannot issue Full"
    );
    let parent_groups = offer
        .proof()
        .unwrap()
        .selection
        .graph_selection()
        .native_groups
        .iter()
        .filter(|group| group.binding.artifact() == a_id && group.original_ordinal == 9001)
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        parent_groups,
        BTreeSet::from([historical_group, current_group])
    );
    assert!(offer
        .custody_snapshot()
        .binding_dependencies()
        .unwrap()
        .contains(&expected_edge));
    let projection = CompilerInputProjection::from_issued_entries(&[entries[1].clone()]).unwrap();
    let metadata = projection
        .project_metadata(current.metadata_snapshot())
        .unwrap();
    let transport =
        NativeCompilerTransport::issue_checked(&projection, &metadata, &current, &[offer]).unwrap();
    let support = transport.availability[&a_id]
        .dependency_support(
            NativeGroupKey {
                artifact: a_id,
                original_ordinal: 9001,
            },
            recovered[0].product.owner(),
        )
        .unwrap();
    assert_eq!(
        support.groups().collect::<BTreeSet<_>>(),
        BTreeSet::from([current_group]),
        "ordinal permission cannot resurrect the unconsumed parent-root binding"
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
    let offer = {
        let inputs = completed.compiler_inputs();
        assert!(inputs.artifacts().selected_native_groups().is_empty());
        let entry = context
            .artifact_view()
            .entries()
            .into_iter()
            .find(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
            .unwrap();
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        let definitions =
            crate::certified_products::original_native_group_definitions(product, 7).unwrap();
        let binding = match &definitions.bindings()[0] {
            tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
            tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
        };
        let imports = [crate::certified_products::PendingImportOwner::Source {
            owner: product.owner().clone(),
            original_ordinal: 7,
            binder: binding.identity.clone(),
        }];
        let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            inputs,
            &inputs.metadata,
            &validation.inventory,
        )
        .unwrap();
        let source = inputs.artifacts();
        let mut fixed = source
            .inventory()
            .bind_segment(source, vec![], &selection)
            .unwrap();
        let target = source
            .inventory()
            .admit_bound_target(
                &mut fixed,
                source,
                crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &selection,
            )
            .unwrap();
        assert_eq!(
            target.artifacts().selected_native_groups(),
            BTreeSet::from([NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: 7,
            }]),
        );
        assert!(inputs.artifacts().selected_native_groups().is_empty());
        let proof =
            NativeOriginalRequirement::from_selected_view(entry.descriptor.id, target.artifacts())
                .unwrap()
                .unwrap();
        IssuedNativeOffer::issue(entry.descriptor.id, target.artifacts(), Some(proof)).unwrap()
    };
    drop(completed);
    drop(context);
    assert!(
        directory_owner.upgrade().is_some(),
        "exact offer custody retains materialized file leases"
    );
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    offer.issued_group_carriers().unwrap();
    drop(offer);
    assert!(
        directory_owner.upgrade().is_none(),
        "the offer must release its last materialization lease"
    );
    assert!(!directory_path.exists());
}

#[test]
fn interface_only_full_availability_preserves_roles_and_admits_later_exact_demand() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("PrivateFullInterface", 121);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &full)
        .unwrap()
        .unwrap();
    let offer = IssuedNativeOffer::issue(entry.descriptor.id, &full, Some(proof)).unwrap();
    assert!(offer.is_full());
    let issued = recover_fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
        full.clone(),
        [],
    )
    .unwrap();
    let interfaces = issued
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![entry.descriptor.owner.clone()])
        .unwrap()
        .retain_offers([offer.clone(), offer.clone()])
        .unwrap();
    // Extending value-type visibility uses the same projected admission seam;
    // an existing private body stays custody, never a namespace contribution.
    let extended = interfaces.admit_value_interfaces(&[]).unwrap();
    assert_eq!(extended.projection(), interfaces.projection());
    assert_eq!(extended.native_offers(), interfaces.native_offers());
    assert_eq!(
        extended.artifacts().selected_native_groups(),
        interfaces.artifacts().selected_native_groups()
    );
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: interfaces,
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));
    let before = context.as_ref().clone();
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation(root.path(), PRODUCER)
        .unwrap();
    assert_eq!(wire_mode(&request, "PrivateFullInterface"), full_mode());
    assert_eq!(
        wire_classification(&request, "PrivateFullInterface"),
        text("private-native")
    );
    assert!(wire_groups(&request)["PrivateFullInterface"].is_empty());
    assert!(request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "PrivateFullInterface")
        .unwrap()
        .product
        .is_none());
    let inputs = request.compiler_inputs();
    assert!(inputs
        .projection()
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert!(inputs.metadata.retained.selected_native_groups.is_empty());
    assert!(inputs.artifacts().selected_native_groups().is_empty());
    assert_eq!(context.as_ref(), &before);
    let owner = native.owner();
    let held_graph = Value::Array(vec![
        Value::Array(vec![]),
        Value::Array(vec![Value::Array(vec![
            text(&owner.unit),
            text(&owner.module),
            text(hex(&owner.module_version.0)),
            text(hex(&owner.skinny_iface_sha256)),
            text(hex(&owner.product_sha256)),
            text(hex(&[7; 32])),
        ])]),
    ]);
    assert!(inputs
        .native_transport
        .metadata
        .entries
        .values()
        .any(|entry| matches!(entry.payload, ArtifactPayload::Original(_))));
    assert!(inputs
        .metadata
        .entries
        .values()
        .all(|entry| matches!(entry.payload, ArtifactPayload::Canonical(_))));
    assert!(compiler_execution_scope(&held_graph, &inputs.metadata)
        .unwrap()
        .as_array()
        .unwrap()[1]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        compiler_execution_scope(&held_graph, &inputs.native_transport.metadata).unwrap(),
        held_graph,
        "private body metadata would incorrectly preserve this replay root if used as compiler policy"
    );
    let validation = PackageInterfaceValidation::default();
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs,
        &inputs.metadata,
        &validation.inventory,
    )
    .unwrap();
    // Exercise the worker's raw receipt boundary, rather than an already
    // authenticated PendingImportOwner supplied directly to target admission.
    {
        use crate::certified_products::{AcceptedGlobal, ReceiptImportOwner};
        use tidepool_repr::execution_schema::{testing, GlobalDecl, RuntimeRep};
        let membership = inputs
            .available_original_sources(std::slice::from_ref(&native), &validation.inventory)
            .unwrap();
        let crate::certified_products::PendingImportOwner::Source { binder, .. } =
            source_import(&native, 9001)
        else {
            unreachable!()
        };
        for case in ["exact", "unversioned", "wrong-version", "wrong-binder"] {
            let mut binder = binder.clone();
            if case == "wrong-binder" {
                binder.occurrence.push_str("_absent");
            }
            let mut wire = testing::wire_program();
            wire.globals = vec![GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            }];
            let prepared = testing::prepare(wire).unwrap();
            let version = match case {
                "unversioned" => None,
                "wrong-version" => Some(tidepool_repr::execution_schema::ModuleVersion([255; 32])),
                _ => Some(native.owner().module_version.clone()),
            };
            let accepted = [AcceptedGlobal {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                owner: ReceiptImportOwner::Source {
                    unit: native.owner().unit.clone(),
                    module: native.owner().module.clone(),
                    module_version: version,
                    original_ordinal: 9001,
                    binder,
                },
            }];
            let result = crate::certified_products::certify_target_available_owners_from_membership_with_validation(&prepared, &accepted, &membership, &request.groups, &selection, &BTreeMap::new(), &mut PackageInterfaceValidation::default());
            if case == "exact" {
                assert_eq!(result.unwrap(), vec![source_import(&native, 9001)]);
            } else {
                let expected = match case {
                    "unversioned" => "unversioned source lacks compiler selection",
                    "wrong-version" => "source outside compiler selection",
                    "wrong-binder" => "source binder/group closure",
                    _ => unreachable!(),
                };
                assert!(
                    matches!(result, Err(crate::certified_products::CertificationError::Mismatch(actual)) if actual == expected),
                    "{case} must refuse at its owning boundary"
                );
            }
        }
    }
    let issued = inputs.native_availability(entry.descriptor.id).unwrap();
    let cloned = selection.clone();
    assert!(cloned
        .issued_native_availability()
        .any(|available| Arc::ptr_eq(available, &issued)));
    assert!(cloned
        .native_original_requirements()
        .any(|proof| proof.original == entry.descriptor.id));
    assert!(cloned.receipt_native_body_owners().next().is_none());
    assert!(cloned
        .complete_native_products(&inputs.artifacts())
        .unwrap()
        .is_empty());
    assert!(!cloned.permits_native_group(ArtifactId([255; 32]), native.owner(), 9001));
    assert!(!cloned.permits_native_group(
        entry.descriptor.id,
        product("WrongPrivateOwner", 128).owner(),
        9001
    ));
    let private = OriginalCompilerInputs::from_selection(&cloned, &inputs.artifacts()).unwrap();
    assert!(private
        .projection
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    let next = ExactCompileContext::new(context.clone())
        .prepare_compilation_with_private_input(
            &root.path().join("next"),
            PRODUCER,
            None,
            Some(private),
        )
        .unwrap();
    assert_eq!(wire_mode(&next, "PrivateFullInterface"), full_mode());
    assert!(wire_groups(&next)["PrivateFullInterface"].is_empty());
    let next_selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        next.compiler_inputs(),
        &next.compiler_inputs().metadata,
        &validation.inventory,
    )
    .unwrap();
    assert!(next_selection.permits_native_group(entry.descriptor.id, native.owner(), 9001));
    let mut fixed = inputs
        .artifacts()
        .inventory()
        .bind_segment(&inputs.artifacts(), vec![], &selection)
        .unwrap();
    let imports = [source_import(&native, 9001)];
    let demanded = inputs
        .artifacts()
        .inventory()
        .admit_bound_target(
            &mut fixed,
            &inputs.artifacts(),
            crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
            vec![],
            &selection,
        )
        .expect("later target uses the same issuer's previously unselected body");
    assert!(demanded.artifacts().selected_native_groups().is_empty());
    let groups = demanded
        .clone()
        .bind_target(None, (&imports).to_vec())
        .resolve()
        .map(|resolved| resolved.groups().clone())
        .unwrap();
    let expected = offer
        .issued_group_carriers()
        .unwrap()
        .into_keys()
        .filter(|group| {
            group.binding.artifact() == entry.descriptor.id && group.original_ordinal == 9001
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(expected.len(), 1);
    assert_eq!(groups.into_iter().collect::<BTreeSet<_>>(), expected);
    assert_eq!(context.as_ref(), &before);
}

#[test]
fn interface_only_partial_union_and_custody_cannot_issue_private_full() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry = Arc::new(
        ArtifactEntry::original(producer.sha256(), product("PrivatePartial", 122)).unwrap(),
    );
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()])
        .unwrap()
        .interface_only();
    let mut partials = Vec::new();
    for ordinal in [7, 9001] {
        let selected = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![entry.clone()],
                &BTreeSet::from([NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: ordinal,
                }]),
            )
            .unwrap();
        let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &selected)
            .unwrap()
            .unwrap();
        let offer = IssuedNativeOffer::issue(entry.descriptor.id, &selected, Some(proof)).unwrap();
        assert!(!offer.is_full());
        partials.push(offer);
    }
    let empty = IssuedNativeOffer::issue(entry.descriptor.id, &full, None).unwrap();
    for offers in [vec![], vec![empty], vec![partials[0].clone()], partials] {
        let metadata = projection
            .project_metadata(full.metadata_snapshot())
            .unwrap();
        let transport =
            NativeCompilerTransport::issue_checked(&projection, &metadata, &full, &offers).unwrap();
        assert!(
            !transport.availability.contains_key(&entry.descriptor.id),
            "neither complete byte custody nor unions of subsets create a Full witness"
        );
        let ArtifactPayload::Original(original) = &entry.payload else {
            unreachable!()
        };
        assert!(
            RequestNativeAvailability(Arc::clone(&transport.availability))
                .receipt_original(
                    &original.owner().unit,
                    &original.owner().module,
                    &original.owner().module_version
                )
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn private_full_availability_requires_exact_namespace_and_refuses_historical_ambiguity() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let natives = [
        product("PrivateFullHistory", 123),
        product("PrivateFullHistory", 124),
    ];
    assert_eq!(natives[0].module_interface(), natives[1].module_interface());
    let entries = natives
        .into_iter()
        .map(|native| Arc::new(ArtifactEntry::original(producer.sha256(), native).unwrap()))
        .collect::<Vec<_>>();
    assert_ne!(entries[0].descriptor.id, entries[1].descriptor.id);
    let inventory = ArtifactInventory::default();
    let canonical = Arc::new(ArtifactEntry::canonical(match &entries[0].payload {
        ArtifactPayload::Original(native) => native.module_interface().unwrap().clone(),
        _ => unreachable!(),
    }));
    let base = inventory
        .admit_shared(&inventory.empty_view(), vec![canonical.clone()])
        .unwrap();
    let views = entries
        .iter()
        .map(|entry| inventory.admit_shared(&base, vec![entry.clone()]).unwrap())
        .collect::<Vec<_>>();
    let offers = entries
        .iter()
        .zip(&views)
        .map(|(entry, view)| {
            let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, view)
                .unwrap()
                .unwrap();
            let offer = IssuedNativeOffer::issue(entry.descriptor.id, view, Some(proof)).unwrap();
            assert!(offer.is_full());
            offer
        })
        .collect::<Vec<_>>();
    let projection = CompilerInputProjection::from_issued_entries(&[canonical]).unwrap();
    let custody = base
        .retain_custody(&views[0])
        .unwrap()
        .retain_custody(&views[1])
        .unwrap();
    for offer in &offers {
        assert!(offer
            .qualified_private_availability(&projection, &custody)
            .is_some());
        assert!(offer
            .qualified_private_availability(&projection, &inventory.empty_view())
            .is_none());
        assert!(offer
            .qualified_private_availability(&CompilerInputProjection::default(), &custody)
            .is_none());
    }
    let metadata = projection
        .project_metadata(custody.metadata_snapshot())
        .unwrap();
    for reversed in [false, true] {
        let selected = if reversed {
            vec![offers[1].clone(), offers[0].clone()]
        } else {
            offers.clone()
        };
        let error =
            NativeCompilerTransport::issue_checked(&projection, &metadata, &custody, &selected)
                .err()
                .expect("type-only role cannot choose among distinct qualified historical Fulls");
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { ref owner }
                if owner == &entries[0].descriptor.owner)));
    }
    let duplicate = NativeCompilerTransport::issue_checked(
        &projection,
        &metadata,
        &custody,
        &[offers[0].clone(), offers[0].clone()],
    )
    .unwrap();
    assert!(duplicate.availability[&entries[0].descriptor.id].permits(9001));
    let original = duplicate.availability[&entries[0].descriptor.id].original_owner();
    let private = RequestNativeAvailability(Arc::clone(&duplicate.availability));
    let receipt_owner = private
        .receipt_original(&original.unit, &original.module, &original.module_version)
        .unwrap()
        .unwrap();
    assert_eq!(receipt_owner.original_owner(), original);
    assert!(receipt_owner.permits(9001));
    for index in 0..2 {
        let selected =
            CompilerInputProjection::from_issued_entries(&[entries[index].clone()]).unwrap();
        let view = views[index].retain_custody(&views[1 - index]).unwrap();
        let metadata = selected.project_metadata(view.metadata_snapshot()).unwrap();
        let transport =
            NativeCompilerTransport::issue_checked(&selected, &metadata, &view, &offers).unwrap();
        assert!(transport.availability[&entries[index].descriptor.id].permits(9001));
        assert!(!transport
            .availability
            .contains_key(&entries[1 - index].descriptor.id));
    }
    let wrong = Arc::new(ArtifactEntry::canonical(fixture_module_core(
        match &entries[0].payload {
            ArtifactPayload::Original(native) => native.module_interface().unwrap(),
            _ => unreachable!(),
        },
        b"different canonical Core".to_vec(),
    )));
    let wrong_projection = CompilerInputProjection::from_issued_entries(&[wrong.clone()]).unwrap();
    let wrong_view = inventory
        .admit_shared(&inventory.empty_view(), vec![wrong])
        .unwrap()
        .retain_custody(&custody)
        .unwrap();
    for offer in &offers {
        assert!(offer
            .qualified_private_availability(&wrong_projection, &wrong_view)
            .is_none());
        assert!(offer
            .qualified_private_availability(&projection, &wrong_view)
            .is_none());
    }
}

#[test]
fn equal_payload_full_offers_preserve_distinct_carrier_and_group_issuance() {
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement,
    };
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("EqualPrivateFull", 125);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let canonical = Arc::new(ArtifactEntry::canonical(
        native.module_interface().unwrap().clone(),
    ));
    for shared_carrier in [false, true] {
        let inventory = ArtifactInventory::default();
        let base = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![if shared_carrier {
                    entry.clone()
                } else {
                    canonical.clone()
                }],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let issue = |marker: &str, version| {
            inventory
                .admit_recovery_selection(
                    &base,
                    vec![
                        entry.clone(),
                        Arc::new(ArtifactEntry::canonical(
                            product(marker, version).module_interface().unwrap().clone(),
                        )),
                    ],
                    &BTreeSet::from([7, 9001].map(|original_ordinal| NativeGroupKey {
                        artifact: entry.descriptor.id,
                        original_ordinal,
                    })),
                )
                .unwrap()
        };
        let views = [issue("EqualFullLeft", 126), issue("EqualFullRight", 127)];
        let offers = views
            .iter()
            .map(|view| {
                let proof = NativeOriginalRequirement::from_issued_group_custody(
                    entry.descriptor.id,
                    canonical.descriptor.id,
                    view,
                )
                .unwrap()
                .unwrap();
                let offer =
                    IssuedNativeOffer::issue(entry.descriptor.id, view, Some(proof)).unwrap();
                assert!(offer.is_full());
                offer
            })
            .collect::<Vec<_>>();
        let projection =
            CompilerInputProjection::from_issued_entries(&[canonical.clone()]).unwrap();
        let namespace = recover_fixture_compiler_support(projection.clone(), base.clone(), [])
            .unwrap()
            .finalize_namespace()
            .unwrap()
            .artifacts()
            .retain_custody(&views[0])
            .unwrap()
            .retain_custody(&views[1])
            .unwrap();
        let witnesses = offers
            .iter()
            .map(|offer| {
                offer
                    .qualified_private_availability(&projection, &namespace)
                    .expect("same selected canonical supports each genuine Full in isolation")
            })
            .collect::<Vec<_>>();
        assert_ne!(
            witnesses[0], witnesses[1],
            "equal payload cannot identify exact carrier/group issuance"
        );
        let metadata = projection
            .project_metadata(namespace.metadata_snapshot())
            .unwrap();
        for selected in [offers.clone(), vec![offers[1].clone(), offers[0].clone()]] {
            let transport = NativeCompilerTransport::issue_checked(
                &projection,
                &metadata,
                &namespace,
                &selected,
            )
            .unwrap();
            let available = &transport.availability[&entry.descriptor.id];
            assert!(available.permits(9001));
            assert_eq!(available.offers().len(), 2);
            for offer in &offers {
                assert!(available.offers().any(|available| available == offer));
            }
            let support =
                recover_fixture_compiler_support(projection.clone(), namespace.clone(), [])
                    .unwrap()
                    .retain_offers(selected)
                    .unwrap();
            let context = Arc::new(ExactDeclarationContext::from_facts(
                DeclarationContextFacts {
                    producer: producer.sha256(),
                    compiler_support: support,
                    lexical: Default::default(),
                    template_imports: None,
                    original_instance_environment: OriginalInstanceEnvironment::Unknown,
                },
            ));
            let root = tempfile::tempdir().unwrap();
            let request = ExactCompileContext::new(context)
                .prepare_compilation(root.path(), PRODUCER)
                .unwrap();
            assert_eq!(wire_mode(&request, "EqualPrivateFull"), full_mode());
            assert!(wire_groups(&request)["EqualPrivateFull"].is_empty());
            let inputs = request.compiler_inputs();
            assert!(inputs
                .projection()
                .roles()
                .iter()
                .all(|role| role.original().is_none()));
            assert!(inputs.artifacts().selected_native_groups().is_empty());
            let validation = PackageInterfaceValidation::default();
            let selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                    inputs,
                    &inputs.metadata,
                    &validation.inventory,
                )
                .unwrap();
            let mut fixed = inputs
                .artifacts()
                .inventory()
                .bind_segment(&inputs.artifacts(), vec![], &selection)
                .unwrap();
            let imports = [source_import(&native, 9001)];
            let demanded = inputs.artifacts().inventory().admit_bound_target(
                &mut fixed,
                &inputs.artifacts(),
                NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &selection,
            );
            assert!(
                matches!(demanded, Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == entry.descriptor.id)),
                "availability preserves alternatives until an exact demand needs a unique binding"
            );
        }
        if !shared_carrier {
            let namespace = views[0].retain_custody(&views[1]).unwrap();
            assert!(offers[0]
                .qualified_private_availability(&projection, &namespace)
                .is_some());
            assert!(offers[1]
                .qualified_private_availability(&projection, &namespace)
                .is_none());
            let metadata = projection
                .project_metadata(namespace.metadata_snapshot())
                .unwrap();
            let transport =
                NativeCompilerTransport::issue_checked(&projection, &metadata, &namespace, &offers)
                    .unwrap();
            assert!(transport.availability[&entry.descriptor.id].permits(9001));
        }
    }
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
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let no_demand = projection
        .project_metadata(base.metadata_snapshot())
        .unwrap();
    for other in [false, true] {
        let mut offers = vec![unqualified.clone()];
        if other {
            offers.push(complete.clone());
        }
        let transport =
            NativeCompilerTransport::issue_checked(&projection, &no_demand, &full, &offers)
                .unwrap();
        assert_eq!(
            transport.availability[&entry.descriptor.id].permits(7),
            other
        );
        assert!(!offers[0].is_full());
    }
    let empty_offer =
        crate::artifact_inventory::IssuedNativeOffer::issue(entry.descriptor.id, &full, None)
            .unwrap();
    let empty =
        NativeCompilerTransport::issue_checked(&projection, &no_demand, &full, &[empty_offer])
            .unwrap();
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
        let projection =
            CompilerInputProjection::from_issued_entries(&[entries[1].clone(), entries[2].clone()])
                .unwrap();
        let metadata = projection
            .project_metadata(view.metadata_snapshot())
            .unwrap();
        let error = NativeCompilerTransport::issue_checked(
            &projection,
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
            .owner
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
        .owner
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
            .projection()
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
        context.owner.facts.original_instance_environment,
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
            .owner
            .facts
            .compiler_support
            .artifacts()
            .inventory()
            .admit_shared(
                &ambiguous.owner.facts.compiler_support.artifacts(),
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
        .owner
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
        .admit_interfaces_under_unissued_policy(
            ambiguous.artifact_view(),
            vec![entries[0].clone()],
            &[],
        )
        .expect("the inventory issuer selects the explicit original and canonical pair");
    let first_id = entries[0].descriptor.id;
    let second_id = entries[1].descriptor.id;
    assert_ne!(first_id, second_id);
    assert_eq!(
        selected
            .selected_native_groups()
            .iter()
            .map(|group| group.artifact)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_id, second_id]),
        "the broad fixture selected both complete native censuses"
    );
    let selected = selected.select_roots(vec![first_id]).unwrap();
    assert_eq!(
        selected
            .selected_native_groups()
            .iter()
            .map(|group| group.artifact)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_id]),
        "the explicit winner alone supplies executable demand"
    );
    let retained_originals = selected
        .entries()
        .into_iter()
        .filter(|entry| {
            matches!(
                &entry.payload,
                crate::artifact_inventory::ArtifactPayload::Original(_)
            )
        })
        .map(|entry| entry.descriptor.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(retained_originals, BTreeSet::from([first_id, second_id]));
    assert!(selected
        .metadata_snapshot()
        .ambiguous_native_owners
        .contains(&identity("fixture", "AmbiguousNative")));
    let support = recover_fixture_compiler_support(
        projection.clone(),
        selected,
        ambiguous
            .compiler_source_support()
            .native_offers()
            .iter()
            .cloned(),
    )
    .unwrap();
    ambiguous = ambiguous.rebuild(|facts| facts.compiler_support = support);
    assert_eq!(ambiguous.compiler_input_projection(), &projection);
    OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .expect("explicit issued original remains usable beside historical custody");
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
            inputs.issued_artifacts().capture_graph_selection().roots,
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
        assert_eq!(carried.offers().collect::<Vec<_>>(), vec![&offer]);
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
                .owner
                .facts
                .compiler_support
                .artifacts()
                .inventory()
                .admit_shared_with_demand(
                    &context.owner.facts.compiler_support.artifacts(),
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
            .owner
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
            .admit_interfaces_under_unissued_policy(
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
        let selected_context = context.clone();
        // The inventory issuer explicitly selects complete original censuses;
        // the compiler issuer still names exactly one of those native bodies.
        {
            let updated = context
                .owner
                .facts
                .compiler_support
                .artifacts()
                .inventory()
                .admit_shared(&context.owner.facts.compiler_support.artifacts(), entries)
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
            .owner
            .facts
            .compiler_support
            .artifacts()
            .selected_native_groups();
        assert_eq!(groups.len(), products.len() * 2);
        let dependencies = context
            .owner
            .facts
            .compiler_support
            .artifacts()
            .dependencies();
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
                .owner
                .facts
                .compiler_support
                .artifacts()
                .selected_native_groups(),
            groups
        );
        assert_eq!(
            context
                .owner
                .facts
                .compiler_support
                .artifacts()
                .dependencies(),
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
        assert!(
            OriginalCompilerInputs::from_selected_authored_declarations(
                &canonical_only,
                producer,
                std::slice::from_ref(&products[1]),
            )
            .is_err(),
            "a genuine unique authored private claim cannot be replaced by a fresh same-owner candidate",
        );
        let other = OriginalCompilerInputs::from_native_availability(
            &type_context(std::slice::from_ref(&products[1])),
            producer,
            std::slice::from_ref(&products[1]),
        )
        .unwrap();
        assert!(other.native_offers().iter().any(|offer| offer.is_full()));
        for reverse in [false, true] {
            let before = selected_context.artifact_view().selected_native_groups();
            let mut historical = selected_context.clone();
            let mut offers = historical
                .compiler_source_support()
                .native_offers()
                .to_vec();
            offers.extend(other.native_offers().iter().cloned());
            if reverse {
                offers.reverse();
            }
            let updated = historical
                .compiler_source_support()
                .retain_offers(offers)
                .unwrap();
            historical = historical.rebuild(|facts| facts.compiler_support = updated);
            assert!(
                before.is_empty(),
                "the selected authored owner has explicit zero demand"
            );
            let inventory = historical.artifact_view().inventory();
            let before_origin = inventory.metrics();
            let direct = OriginalCompilerInputs::reuse_selected_authored(&historical)
                .unwrap()
                .expect("catalog-absent authored reuse carries its selected exact owner");
            let after_origin = inventory.metrics();
            assert_eq!(after_origin.graph_visits, before_origin.graph_visits);
            assert_eq!(
                after_origin.entry_handle_copies,
                before_origin.entry_handle_copies
            );
            // Observe the old route only after the consumer: its graph work
            // proves this history supplied a cold view, not a warmed no-op.
            let metadata = historical.artifact_view().metadata_snapshot();
            assert!(inventory.metrics().graph_visits > after_origin.graph_visits);
            let selected_role = historical
                .compiler_input_projection()
                .snapshot_roles()
                .get(&identity("main", &module))
                .unwrap()
                .clone();
            assert!(matches!(
                &metadata.artifacts[&selected_role.interface()].payload,
                ArtifactPayload::Canonical(interface)
                    if interface == current.module_interface().unwrap()
            ));
            assert_eq!(direct.artifacts.selected_native_groups(), before);
            assert!(direct.issued_input_decisions().is_some());
            assert_eq!(
                direct
                    .projection
                    .snapshot_roles()
                    .get(&identity("main", &module))
                    .unwrap()
                    .original(),
                Some(current_native),
            );
            assert!(direct
                .native_offers()
                .iter()
                .any(|offer| offer.original() == other.native_offers()[0].original()));
            let mixed = OriginalCompilerInputs::from_selected_authored_declarations(
                &historical,
                producer,
                std::slice::from_ref(current),
            )
            .unwrap()
            .unwrap();
            assert_eq!(mixed.artifacts.selected_native_groups(), before);
            assert_eq!(mixed.projection, direct.projection);
        }
        for reverse in [false, true] {
            let mut offers = other.native_offers().to_vec();
            if reverse {
                offers.reverse();
            }
            let mut competing = canonical_only.clone();
            let updated = competing
                .compiler_source_support()
                .retain_offers(offers)
                .unwrap();
            competing = competing.rebuild(|facts| facts.compiler_support = updated);
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
            .owner
            .facts
            .compiler_support
            .artifacts()
            .inventory()
            .admit_shared_with_demand(
                &context.owner.facts.compiler_support.artifacts(),
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
                .owner
                .facts
                .compiler_support
                .artifacts()
                .selected_native_groups(),
            groups
        );
        assert_eq!(
            context
                .owner
                .facts
                .compiler_support
                .artifacts()
                .dependencies(),
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
        .owner
        .facts
        .compiler_support
        .artifacts()
        .selected_native_groups();
    let assert_exact_offer = |context: &ExactDeclarationContext, name: &str, expected: bool| {
        let before = context
            .owner
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
                .owner
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
        .owner
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
        .owner
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
            .owner
            .facts
            .compiler_support
            .artifacts()
            .inventory()
            .admit_recovery_selection(
                &partial
                    .owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .inventory()
                    .empty_view(),
                partial.owner.facts.compiler_support.artifacts().entries(),
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
                .owner
                .facts
                .compiler_support
                .artifacts()
                .selected_native_groups(),
            selected
        );
        let private =
            OriginalCompilerInputs::from_selected_authored_declarations(&partial, producer, &[])
                .unwrap();
        assert!(private.as_ref().is_none_or(|private| {
            private
                .projection
                .roles()
                .iter()
                .all(|role| role.original() != Some(authored_root))
        }));
        assert_exact_offer(
            &partial,
            &format!("partial-authored-exact-offer-{selection_index}"),
            false,
        );
        if omitted.is_some()
            && partial
                .owner
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
    assert_eq!(
        wire_classification(&request, "UnavailableOriginal"),
        text("compiler-original")
    );
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
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement,
    };
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
    eprintln!("native transport history: {history:?}");
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
                assert_eq!(
                    after, before,
                    "custody expansion cannot change supplied evidence with fixed canonical bindings"
                );
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
        let context = context(&sources[index]);
        let offer = crate::artifact_inventory::IssuedNativeOffer::issue(
            proofs[index].original,
            &sources[index],
            Some(proofs[index].clone()),
        )
        .unwrap();
        let support = context
            .compiler_source_support()
            .retain_offers([offer])
            .unwrap();
        let context = context
            .as_ref()
            .clone()
            .rebuild(|facts| facts.compiler_support = support);
        OriginalCompilerInputs::from_native_availability(
            &context,
            producer,
            std::slice::from_ref(&products[index + 1]),
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
    // The private product constructor deliberately has no full issuer view.
    // Eager-fold equivalence is covered by the independently issued history
    // above; this history follows the real opaque-owner consumer path.
    let before_retention = base_context.artifact_view().capture_graph_selection();
    let retained =
        retain_native_offers(base_context.artifact_view(), &joined.native_offers).unwrap();
    let after_retention = retained.capture_graph_selection();
    assert_eq!(after_retention.roots, before_retention.roots);
    assert_eq!(after_retention.namespace, before_retention.namespace);
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
    let roles = inputs.projection().snapshot_roles();
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
                && proof.issued_group_carriers(inputs.artifacts()).unwrap() == expected[index]
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
            .artifacts()
            .inventory()
            .bind_segment(inputs.artifacts(), vec![], &selected)
    };
    for index in 0..2 {
        let mut fixed = bind().unwrap();
        let imports = [source_import(&products[index + 1], 7)];
        let admitted = inputs.artifacts().inventory().admit_bound_target(
            &mut fixed,
            inputs.artifacts(),
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
                .clone()
                .bind_target(None, (&imports).to_vec())
                .resolve()
                .map(|resolved| resolved.groups().clone())
                .unwrap();
            let expected_index = if same_root { 0 } else { index };
            assert_eq!(groups, expected[expected_index].keys().copied().collect());
            coverage.successful_demands += 1;
        }
    }
    let mut fixed = bind().unwrap();
    let imports = [source_import(&products[0], 7)];
    let demanded = inputs.artifacts().inventory().admit_bound_target(
        &mut fixed,
        inputs.artifacts(),
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
            .clone()
            .bind_target(None, (&imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
            .unwrap();
        assert_eq!(groups, BTreeSet::from([child(0).0]));
        coverage.successful_demands += 1;
    }
    let handoff = OriginalCompilerInputs::from_selection(&selected, inputs.artifacts()).unwrap();
    let next = ExactCompileContext::new(base_context.clone())
        .prepare_compilation_with_private_input(
            &root.path().join("handoff"),
            PRODUCER,
            None,
            Some(handoff),
        )
        .unwrap();
    let encoded = serde_json::to_vec(&(
        next.compiler_inputs().artifacts().capture_graph_selection(),
        next.compiler_inputs().artifacts().binding_dependencies(),
    ))
    .unwrap();
    let (graph, edges): (ArtifactGraphSelection, Vec<_>) =
        serde_json::from_slice(&encoded).unwrap();
    let recovery = ArtifactInventory::default();
    assert_eq!(recovery.node_count(), 0);
    let restored = recovery
        .recover_selection(
            &recovery.empty_view(),
            next.compiler_inputs().artifacts().entries(),
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
    let actual_context = context(&base);
    let actual_request = ProtectedScaffoldRequirements::default()
        .compiler_inputs(&actual_context, Some(&private[0]))
        .unwrap();
    let produced = OriginalCompilerInputs::from_checked_support_products(
        &actual_request.shared_compiler_source_support(),
        producer.sha256(),
        std::slice::from_ref(&dependent),
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
    let dependent_offer = produced
        .native_offers()
        .iter()
        .find(|offer| offer.original() == dependent_entry.descriptor.id)
        .unwrap()
        .clone();
    assert!(dependent_offer.is_full());
    assert_eq!(dependent_offer.proof(), Some(&proof));
    assert!(
        proof
            .issued_group_carriers(produced.issued_artifacts())
            .is_err(),
        "carrier-only output cannot authenticate the opaque dependent Group proof"
    );
    let expected_child = *expected[0]
        .keys()
        .find(|group| {
            group.binding.artifact() == entries[1].descriptor.id && group.original_ordinal == 7
        })
        .unwrap();
    let expected_dependency = (
        ArtifactBindingNode::Group(*dependent_group),
        ArtifactBindingNode::Group(expected_child),
        crate::artifact_inventory::ArtifactDependency::NativeGroup {
            dependent_ordinal: 7,
            required_ordinal: 7,
        },
    );
    let assert_dependent_offer = |offers: &[IssuedNativeOffer]| {
        let retained = offers
            .iter()
            .find(|offer| offer.same_issuance(&dependent_offer))
            .expect("the exact dependent issuance retains its historical child");
        assert!(retained.is_full());
        assert_eq!(retained.proof(), Some(&proof));
        assert_eq!(
            retained.issued_group_carriers().unwrap(),
            dependent_expected
        );
        let edges = retained.custody_snapshot().binding_dependencies().unwrap();
        assert!(edges.contains(&(
            ArtifactBindingNode::Group(*dependent_group),
            ArtifactBindingNode::Artifact(dependent_carrier),
            crate::artifact_inventory::ArtifactDependency::Interface,
        )));
        assert!(edges.contains(&expected_dependency));
    };
    assert_dependent_offer(produced.native_offers());
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
    assert!(proof
        .issued_group_carriers(continued.issued_artifacts())
        .is_err());
    assert_dependent_offer(continued.native_offers());
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
                continued_inputs.artifacts().capture_graph_selection(),
                continued_inputs.artifacts().binding_dependencies(),
            ))
            .unwrap(),
        )
        .unwrap();
    let fresh = ArtifactInventory::default();
    assert_eq!(fresh.node_count(), 0);
    let reopened = fresh
        .recover_selection(
            &fresh.empty_view(),
            continued_inputs.artifacts().entries(),
            &continued_graph,
            &continued_edges,
        )
        .unwrap();
    coverage.recoveries += 1;
    assert_dependent_offer(continued_inputs.compiler_source_support().native_offers());
    for view in [continued_inputs.artifacts(), &reopened] {
        assert!(
            proof.issued_group_carriers(view).is_err(),
            "selected carrier snapshots do not reconstruct opaque Group availability"
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
            .clone()
            .bind_target(None, (&imports).to_vec())
            .resolve()
            .map(|resolved| resolved.groups().clone())
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
            native_availability: RequestNativeAvailability::issue_checked(
                &projection,
                &custody,
                std::slice::from_ref(&offer),
            )
            .unwrap(),
            demand: crate::declaration_context::OriginalInputDemand::RolePolicy,
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
                .artifacts()
                .inventory()
                .bind_segment(inputs.artifacts(), vec![], &selection)
                .unwrap();
            let imports = [source_import(&native, demanded)];
            let admitted = inputs.artifacts().inventory().admit_bound_target(
                &mut fixed,
                inputs.artifacts(),
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
            .artifacts()
            .inventory()
            .bind_segment(inputs.artifacts(), vec![], &selection)
            .unwrap();
        let imports = [source_import(&unrelated, 7)];
        let denied = inputs.artifacts().inventory().admit_bound_target(
            &mut fixed,
            inputs.artifacts(),
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
    let empty_graph = empty.custody_snapshot().graph_selection().unwrap();
    let empty_ids = empty_graph
        .bindings
        .iter()
        .map(|binding| binding.artifact)
        .collect::<BTreeSet<_>>();
    assert!(empty_ids.contains(&entry.descriptor.id));
    assert!(empty_ids.contains(&canonical));
    assert!(empty.issued_group_carriers().unwrap().is_empty());
    assert!(empty_graph.native_groups.is_empty());
    assert!(empty_graph.roots.is_empty());
    assert!(empty_graph.namespace.is_empty());
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
        for interface_only in [false, true] {
            let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: recover_fixture_compiler_support(
                    if interface_only {
                        projection.interface_only()
                    } else {
                        projection.clone()
                    },
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
            let mut restored = recovered
                .context_for_expected_snapshot(receipt.root(), &receipt)
                .unwrap();
            restored.bind_snapshot_baseline(&receipt).unwrap();
            let restored_offer = &restored.owner.facts.compiler_support.native_offers()[0];
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
                .prepare_compilation(
                    &root
                        .path()
                        .join(format!("restored-{index}-{interface_only}")),
                    PRODUCER,
                )
                .unwrap();
            if interface_only && index != 0 {
                assert!(!request
                    .compiler_inputs()
                    .native_transport
                    .availability
                    .contains_key(&entry.descriptor.id));
            } else {
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
            }
            assert!(request
                .compiler_inputs()
                .projection()
                .roles()
                .iter()
                .all(|role| !interface_only || role.original().is_none()));
            if interface_only {
                let inputs = request.compiler_inputs();
                let validation = PackageInterfaceValidation::default();
                let selection =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                        inputs,
                        &inputs.metadata,
                        &validation.inventory,
                    )
                    .unwrap();
                let ArtifactPayload::Original(native) = &entry.payload else {
                    unreachable!()
                };
                assert_eq!(
                    selection.permits_native_group(entry.descriptor.id, native.owner(), 9001),
                    index == 0
                );
                assert!(selection.receipt_native_body_owners().next().is_none());
                if index == 0 {
                    let mut fixed = inputs
                        .artifacts()
                        .inventory()
                        .bind_segment(&inputs.artifacts(), vec![], &selection)
                        .unwrap();
                    let imports = [source_import(native, 9001)];
                    let demanded = inputs
                        .artifacts()
                        .inventory()
                        .admit_bound_target(
                            &mut fixed,
                            &inputs.artifacts(),
                            crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(
                                &imports,
                            ),
                            vec![],
                            &selection,
                        )
                        .expect("restored private Full survives certification and actual demand");
                    assert!(demanded.artifacts().selected_native_groups().is_empty());
                    let groups = demanded
                        .clone()
                        .bind_target(None, (&imports).to_vec())
                        .resolve()
                        .map(|resolved| resolved.groups().clone())
                        .unwrap();
                    let expected = offer
                        .issued_group_carriers()
                        .unwrap()
                        .into_keys()
                        .filter(|group| {
                            group.binding.artifact() == entry.descriptor.id
                                && group.original_ordinal == 9001
                        })
                        .collect::<BTreeSet<_>>();
                    assert_eq!(expected.len(), 1);
                    assert_eq!(groups.into_iter().collect::<BTreeSet<_>>(), expected);
                }
            }
            assert_eq!(
                request.context().artifact_view().selected_native_groups(),
                context.artifact_view().selected_native_groups()
            );
        }
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
    let left_graph = left_offer.custody_snapshot().graph_selection().unwrap();
    let right_graph = right_offer.custody_snapshot().graph_selection().unwrap();
    assert_eq!(
        left_graph
            .bindings
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>(),
        right_graph
            .bindings
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>(),
        "identical payload membership cannot identify issuance",
    );
    assert!(left_graph.native_groups.is_empty());
    assert!(right_graph.native_groups.is_empty());
    assert_ne!(
        left_offer, right_offer,
        "scoped Empty facts survive live offer deduplication"
    );
    assert_ne!(left_graph, right_graph);
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
    let mut restored = recovered
        .context_for_expected_snapshot(receipt.root(), &receipt)
        .unwrap();
    restored.bind_snapshot_baseline(&receipt).unwrap();
    let expected = [left_offer.snapshot_facts(), right_offer.snapshot_facts()]
        .into_iter()
        .map(|fact| (fact.identity().unwrap(), fact))
        .collect::<BTreeMap<_, _>>();
    let actual = restored
        .owner
        .facts
        .compiler_support
        .native_offers()
        .iter()
        .map(|offer| {
            assert!(offer.proof().is_none());
            assert!(!offer.is_full());
            assert!(offer.custody_snapshot().roots.is_empty());
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
    assert!(issued.owner.facts.compiler_support.native_offers()[0].is_full());
    assert_eq!(issued.artifact_view().selected_native_groups().len(), 2);
    let projection = issued.owner.facts.compiler_support.projection().clone();
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
    let full = issued.owner.facts.compiler_support.native_offers()[0].clone();
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
        let mut restored = recovered
            .context_for_expected_snapshot(receipt.root(), &receipt)
            .unwrap();
        restored.bind_snapshot_baseline(&receipt).unwrap();
        assert_eq!(
            restored.owner.facts.compiler_support.native_offers()[0].snapshot_facts(),
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
        for (changed_body, reuse_canonical) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            if reuse_canonical && changed_canonical {
                continue;
            }
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
                        let partial_view = sources[0]
                            .inventory()
                            .admit_recovery_selection(
                                &sources[0].inventory().empty_view(),
                                vec![entries[0].clone()],
                                &BTreeSet::from([NativeGroupKey {
                                    artifact: entries[0].descriptor.id,
                                    original_ordinal: 7,
                                }]),
                            )
                            .unwrap();
                        let partial_proof = NativeOriginalRequirement::from_selected_view(
                            entries[0].descriptor.id,
                            &partial_view,
                        )
                        .unwrap()
                        .unwrap();
                        let partial = IssuedNativeOffer::issue(
                            entries[0].descriptor.id,
                            &partial_view,
                            Some(partial_proof),
                        )
                        .unwrap();
                        assert!(!partial.is_full());
                        assert_eq!(
                            partial
                                .issued_group_carriers()
                                .unwrap()
                                .keys()
                                .map(|group| group.original_ordinal)
                                .collect::<BTreeSet<_>>(),
                            BTreeSet::from([7])
                        );
                        assert!(full.is_full());
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
                        let selected_index = if reuse_canonical { 0 } else { 1 };
                        let issued = recover_fixture_compiler_support(
                            CompilerInputProjection::from_issued_entries(&[entries
                                [selected_index]
                                .clone()])
                            .unwrap(),
                            sources[selected_index].clone(),
                            [],
                        )
                        .unwrap();
                        let selected_support = issued
                            .selected_interfaces()
                            .unwrap()
                            .support_for_owners(vec![entries[1].descriptor.owner.clone()])
                            .unwrap();
                        assert!(
                            selected_support.native_offers().is_empty(),
                            "interface observation does not issue native availability"
                        );
                        let selected = selected_support.artifacts().clone();
                        let projection = selected_support.projection().clone();
                        let canonical_id = ArtifactEntry::canonical(match &entries[0].payload {
                            ArtifactPayload::Original(product) => {
                                product.module_interface().unwrap().clone()
                            }
                            _ => unreachable!(),
                        })
                        .descriptor
                        .id;
                        let old_carriers = sources[0]
                            .capture_graph_selection()
                            .bindings
                            .into_iter()
                            .filter(|binding| binding.artifact == entries[0].descriptor.id)
                            .collect::<Vec<_>>();
                        let [old_carrier] = old_carriers.as_slice() else {
                            panic!("one actual original carrier at issuance")
                        };
                        let old_canonicals = sources[0]
                            .binding_dependencies()
                            .into_iter()
                            .filter_map(|(from, to, dependency)| match (from, to, dependency) {
                                (
                                    ArtifactBindingNode::Artifact(from),
                                    ArtifactBindingNode::Artifact(to),
                                    ArtifactDependency::Interface,
                                ) if from == *old_carrier && to.artifact == canonical_id => {
                                    Some(to)
                                }
                                _ => None,
                            })
                            .collect::<BTreeSet<_>>();
                        assert_eq!(old_canonicals.len(), 1);
                        let old_canonical = *old_canonicals.first().unwrap();
                        let current_namespace = selected.capture_graph_selection().namespace;
                        let same_canonical_binding = current_namespace.contains(&old_canonical);
                        assert!(
                            current_namespace.iter().all(|binding| entries
                                .iter()
                                .all(|entry| binding.artifact != entry.descriptor.id)),
                            "interface namespace does not select either original carrier"
                        );
                        if reuse_canonical {
                            assert!(
                                same_canonical_binding,
                                "positive branch reuses the actual issued canonical owner"
                            );
                        } else if changed_body || changed_canonical {
                            assert!(
                                !same_canonical_binding,
                                "independent changed history has a distinct canonical binding"
                            );
                            if !changed_canonical {
                                assert!(
                                    current_namespace
                                        .iter()
                                        .any(|binding| binding.artifact == canonical_id),
                                    "negative branch has equal canonical bytes with a different issuer"
                                );
                            }
                        } else {
                            assert!(
                                same_canonical_binding,
                                "identical authenticated history restores the same binding"
                            );
                        }
                        eprintln!(
                            "historical availability issuance: changed_canonical={changed_canonical} changed_body={changed_body} reuse_canonical={reuse_canonical} mode={mode} reverse={reverse} duplicate={duplicate} old_canonical={old_canonical:?} selected_namespace={current_namespace:?}"
                        );

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
                            .custody_snapshot()
                            .graph_selection()
                            .unwrap()
                            .bindings
                            .iter()
                            .all(|node| graph.bindings.contains(node)));
                        let retained_offer = support
                            .native_offers()
                            .iter()
                            .find(|retained| retained.snapshot_facts() == offer.snapshot_facts())
                            .expect("template support retains the exact opaque issuer");
                        assert_eq!(
                            retained_offer
                                .custody_snapshot()
                                .binding_dependencies()
                                .unwrap(),
                            offer.custody_snapshot().binding_dependencies().unwrap()
                        );
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
                            assert!(
                                ExactCompileContext::new(Arc::new(incompatible))
                                    .prepare_compilation(
                                        &root.path().join(format!("incompatible-{histories}")),
                                        PRODUCER,
                                    )
                                    .is_err(),
                                "historical issuance cannot bypass current native role compatibility"
                            );
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
                        let restored = recovered
                            .context_for_expected_snapshot(receipt.root(), &receipt)
                            .unwrap();
                        assert_eq!(
                            restored
                                .owner
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
                            .artifacts()
                            .selected_native_groups()
                            .is_empty());
                        assert_eq!(request.compiler_inputs().projection(), &projection);
                        let availability = &request.compiler_inputs().native_transport.availability;
                        if mode == 2 && same_canonical_binding {
                            assert_eq!(
                                availability.keys().copied().collect::<BTreeSet<_>>(),
                                BTreeSet::from([entries[0].descriptor.id])
                            );
                            let original = &availability[&entries[0].descriptor.id];
                            assert!(matches!(original.mode, NativeTransportMode::Full));
                            assert!(original.permits(9001));
                            assert_eq!(
                                original.original_owner().module_version,
                                match &entries[0].payload {
                                    ArtifactPayload::Original(product) =>
                                        product.owner().module_version.clone(),
                                    _ => unreachable!(),
                                }
                            );
                        } else {
                            assert!(
                                availability.is_empty(),
                                "Partial, Empty, and incompatible canonical offers do not issue private Full"
                            );
                        }

                        histories += 1;
                    }
                }
            }
        }
    }
    assert_eq!(histories, 72);
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
    let private = OriginalCompilerInputs::from_selection(&selection, inputs.artifacts()).unwrap();
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
        OriginalCompilerInputs::from_selection(&selection, clean.compiler_inputs().artifacts())
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
        request.artifacts().capture_graph_selection().namespace,
        baseline.artifact_view().capture_graph_selection().namespace
    );
    assert!(request.artifacts().selected_native_groups().is_empty());
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
            assert!(before.bindings.iter().all(|binding| {
                retained_again
                    .capture_graph_selection()
                    .bindings
                    .contains(binding)
            }));
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
        let retained = inputs.issued_artifacts().capture_graph_selection();
        assert!(expected_custody
            .bindings
            .iter()
            .all(|binding| retained.bindings.contains(binding)));
        assert!(expected_edges.iter().all(|edge| {
            inputs
                .issued_artifacts()
                .binding_dependencies()
                .contains(edge)
        }));
        assert!(inputs
            .issued_artifacts()
            .selected_native_groups()
            .is_empty());
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
    let parent = dependent_product(
        "HistoricalDependencyParent",
        134,
        vec![(7, vec![source_import(&native, 7)])],
    );
    let parent = recovered_witness_fixtures(&[native.clone(), parent])
        .remove(1)
        .product;
    let parent_entry = Arc::new(ArtifactEntry::original(producer.sha256(), parent).unwrap());
    let support =
        recover_fixture_compiler_support(projection.clone(), left.clone(), [a.clone()]).unwrap();
    let parent_input = OriginalCompilerInputs::from_checked_support_products(
        &support,
        producer.sha256(),
        std::slice::from_ref(match &parent_entry.payload {
            ArtifactPayload::Original(product) => product,
            _ => unreachable!(),
        }),
    )
    .unwrap();
    let parent_offer = parent_input
        .native_offers()
        .iter()
        .find(|offer| offer.original() == parent_entry.descriptor.id)
        .unwrap()
        .clone();
    let groups = parent_offer.issued_group_carriers().unwrap();
    let parent_group = *groups
        .keys()
        .find(|group| {
            group.binding.artifact() == parent_entry.descriptor.id && group.original_ordinal == 7
        })
        .unwrap();
    let historical_child = *a_groups
        .iter()
        .find(|group| group.original_ordinal == 7)
        .unwrap();
    let historical_edge = (
        crate::artifact_inventory::ArtifactBindingNode::Group(parent_group),
        crate::artifact_inventory::ArtifactBindingNode::Group(historical_child),
        crate::artifact_inventory::ArtifactDependency::NativeGroup {
            dependent_ordinal: 7,
            required_ordinal: 7,
        },
    );
    assert!(parent_offer
        .custody_snapshot()
        .binding_dependencies()
        .unwrap()
        .contains(&historical_edge));
    assert!(!b_groups.contains(&historical_child));
    let support = fixture_compiler_support(projection.clone(), right.clone());
    let original_graph = support.artifacts().capture_graph_selection();
    let assert_retained_offer = |support: &crate::artifact_inventory::CompilerSourceSupport,
                                 expected: &IssuedNativeOffer| {
        let retained = support
            .native_offers()
            .iter()
            .find(|retained| retained.same_issuance(expected))
            .expect("the same opaque issuance retains its exact historical custody");
        assert_eq!(
            retained.custody_snapshot().binding_dependencies().unwrap(),
            expected.custody_snapshot().binding_dependencies().unwrap()
        );
    };
    let fresh = product("FreshAlongsideSelectedIssuer", 135);
    for (extra, mixed_fresh) in [
        (vec![b.clone()], false),
        (vec![a.clone(), b.clone()], false),
        (vec![b.clone(), a.clone()], false),
        (vec![a.clone(), b.clone(), parent_offer.clone()], false),
        (vec![a.clone(), b.clone(), parent_offer.clone()], true),
        (vec![b.clone(), a.clone()], true),
    ] {
        let includes_a = extra.iter().any(|offer| offer == &a);
        let includes_parent = extra.iter().any(|offer| offer == &parent_offer);
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
        if includes_parent {
            assert_retained_offer(&carried, &parent_offer);
            assert!(parent_offer
                .custody_snapshot()
                .binding_dependencies()
                .unwrap()
                .contains(&historical_edge));
        }
        if includes_a {
            assert_retained_offer(&carried, &a);
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
        assert_eq!(exact_groups(&baseline.artifacts()), b_groups);
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("native-issuance-does-not-read-manifest"),
            by_owner: BTreeMap::new(),
            native_availability: if mixed_fresh {
                vec![native.clone(), fresh.clone()]
            } else {
                vec![native.clone()]
            },
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let reused = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .expect("reusing a candidate cannot promote historical A into a second root")
            .unwrap();
        assert_eq!(
            exact_groups(&reused.artifacts),
            b_groups,
            "candidate fragment preserves the receiver's independently selected B roots"
        );
        let root = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(context)
            .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(reused))
            .unwrap();
        assert_eq!(
            exact_groups(request.compiler_inputs().artifacts()),
            b_groups
        );
        assert_eq!(
            request
                .compiler_inputs()
                .artifacts()
                .selected_native_groups()
                .len(),
            2,
            "fresh availability cannot add executable demand beside selected B"
        );
        if includes_a {
            assert_retained_offer(request.compiler_inputs().compiler_source_support(), &a);
        }
        if includes_parent {
            assert!(
                request
                    .compiler_inputs()
                    .artifacts()
                    .capture_graph_selection()
                    .roots
                    .iter()
                    .all(|node| match node {
                        crate::artifact_inventory::ArtifactBindingNode::Group(group) =>
                            group.binding.artifact() != parent_entry.descriptor.id,
                        crate::artifact_inventory::ArtifactBindingNode::Artifact(binding) =>
                            binding.artifact != parent_entry.descriptor.id,
                    }),
                "historical parent availability cannot become a current root"
            );
            assert_retained_offer(
                request.compiler_inputs().compiler_source_support(),
                &parent_offer,
            );
            assert!(
                parent_offer
                    .custody_snapshot()
                    .binding_dependencies()
                    .unwrap()
                    .contains(&historical_edge),
                "historical parent retains its exact child without becoming selected demand"
            );
        }
    }
    // Two actual selected decision owners conflict; broader offer custody alone
    // does not create a second selected choice.
    let left_support = fixture_compiler_support(projection.clone(), left.clone());
    let error = left_support.merge(&support).unwrap_err();
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
    let selected_fragment = |view: ArtifactView| {
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: fixture_compiler_support(projection.clone(), view),
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        });
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("selected-fragment-native-issuance-only"),
            by_owner: BTreeMap::new(),
            native_availability: vec![native.clone()],
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .unwrap()
            .unwrap()
    };
    let explicit_a = selected_fragment(left);
    let explicit_b = selected_fragment(right);
    assert_ne!(
        explicit_a.demand, explicit_b.demand,
        "selected demand identity includes exact scoped issuer choices"
    );
    for (left, right) in [(&explicit_a, &explicit_b), (&explicit_b, &explicit_a)] {
        assert!(
            matches!(left.merge(right), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                if artifact == entry.descriptor.id)),
            "two explicit incompatible selected sources must refuse in either order"
        );
    }
}

#[test]
fn candidate_reuse_keeps_published_policy_separate_from_withdrawn_demand() {
    use crate::artifact_inventory::ArtifactBindingNode;
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("PublishedCandidateDemand", 136);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let target = Arc::new(ArtifactEntry::canonical(
        product("PublishedCandidateTarget", 137)
            .module_interface()
            .unwrap()
            .clone(),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone(), target.clone()])
        .unwrap();
    let projection =
        CompilerInputProjection::from_issued_entries(&[entry.clone(), target.clone()]).unwrap();
    let source = ExactDeclarationContext::from_authenticated_execution(
        producer.sha256(),
        &view,
        projection,
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![entry.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: entry.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            entry.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    let publication = source
        .issue_published_source_original("revision", "input", &entry.descriptor.owner)
        .unwrap();
    let published = publication.context();
    let role =
        published.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner].clone();
    let policy =
        crate::artifact_inventory::NativeOriginalRequirement::from_role(role.clone()).unwrap();
    assert!(policy
        .selection
        .graph_selection()
        .roots
        .iter()
        .any(|node| matches!(node, ArtifactBindingNode::Group(_))));
    // Resolve the published role through its real merge issuer, then narrow
    // the closed owner to canonical roots. The empty choice remains issued.
    let resolved = published
        .compiler_source_support()
        .merge(&crate::artifact_inventory::CompilerSourceSupport::empty())
        .unwrap();
    let support = resolved
        .select_support_roots(
            published
                .compiler_input_projection()
                .roles()
                .into_iter()
                .map(|role| role.interface())
                .collect(),
        )
        .unwrap();
    assert!(support.artifacts().selected_native_groups().is_empty());
    assert!(support
        .recorded_input_decisions()
        .values()
        .all(|fact| fact.groups.is_empty()));
    assert!(support
        .recorded_input_decisions()
        .contains_key(&entry.descriptor.owner));
    let context = published
        .as_ref()
        .clone()
        .rebuild(|facts| facts.compiler_support = support);
    assert_eq!(
        context.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner],
        role
    );
    let fresh = product("FreshBesidePublishedZeroDemand", 138);
    for mixed_fresh in [false, true] {
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("candidate-native-issuance-only"),
            by_owner: BTreeMap::new(),
            native_availability: if mixed_fresh {
                vec![native.clone(), fresh.clone()]
            } else {
                vec![native.clone()]
            },
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let reused = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .unwrap()
            .unwrap();
        assert!(reused
            .issued_artifacts()
            .selected_native_groups()
            .is_empty());
        assert!(reused
            .issued_input_decisions()
            .unwrap()
            .recorded_facts()
            .values()
            .all(|fact| fact.groups.is_empty()));
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                published.compiler_input_projection(),
                &published.artifact_view().metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let policy_fragment =
            OriginalCompilerInputs::from_selection(&selection, published.artifact_view()).unwrap();
        let owner = &entry.descriptor.owner;
        let fragment_role = policy_fragment
            .projection
            .snapshot_roles()
            .get(owner)
            .expect("the policy producer has an actual original role");
        assert_eq!(fragment_role.original(), Some(entry.descriptor.id));
        assert_eq!(fragment_role.interface(), role.interface());
        assert!(
            selection.continuation_input(owner).unwrap().is_none(),
            "this producer is NewProduct, not a missing carried input"
        );
        assert!(matches!(
            policy_fragment
                .issued_input_decisions()
                .unwrap()
                .for_role(owner, fragment_role),
            crate::artifact_inventory::InputNativeDecision::UnissuedPolicy
        ));
        let reused_role = reused.projection.snapshot_roles().get(owner).unwrap();
        assert_eq!(reused_role, &role);
        assert!(
            matches!(reused.issued_input_decisions().unwrap()
            .for_role(owner, reused_role), crate::artifact_inventory::InputNativeDecision::IssuedChoices(choices)
                if choices.is_empty()),
            "this exact owner's empty choice remains issued"
        );
        for (left, right) in [(&reused, &policy_fragment), (&policy_fragment, &reused)] {
            let merged = left.merge(right).unwrap();
            assert!(
                merged
                    .issued_artifacts()
                    .selected_native_groups()
                    .is_empty(),
                "an explicit empty source cannot fall back to the published role policy in either order"
            );
            assert!(merged
                .issued_input_decisions()
                .unwrap()
                .recorded_facts()
                .values()
                .all(|fact| fact.groups.is_empty()));
        }
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context.clone()))
            .prepare_compilation_with_private_input(directory.path(), PRODUCER, None, Some(reused))
            .unwrap();
        assert!(
            request
                .compiler_inputs()
                .artifacts()
                .selected_native_groups()
                .is_empty(),
            "published policy cannot resurrect withdrawn source demand"
        );
        assert_eq!(
            request.compiler_inputs().projection().snapshot_roles()[&entry.descriptor.owner],
            role
        );
    }
}

#[test]
fn authored_fallback_does_not_requalify_unrelated_selected_published_owner() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let authored = |generation, version| {
        let module =
            tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation)).module_name();
        let native = crate::certified_products::fixture_native_declaration_product(
            crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
                "main",
                &module,
                vec![(7, vec![]), (9001, vec![])],
                version,
                &BTreeMap::new(),
                b"same authored canonical interface".to_vec(),
                &BTreeMap::new(),
                &[],
            ),
            producer.sha256(),
            generation,
        );
        recovered_witness_fixtures(&[native]).remove(0).product
    };
    let current = product("PublishedFallbackOwner", 151);
    let historical = product("PublishedFallbackOwner", 152);
    let fallback = authored(2, 153);
    assert!(matches!(
        current.module_interface().unwrap().origin(),
        crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
    ));
    assert!(matches!(
        historical.module_interface().unwrap().origin(),
        crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
    ));
    assert!(matches!(
        fallback.module_interface().unwrap().origin(),
        crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
    ));
    assert_eq!(current.module_interface(), historical.module_interface());
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), current.clone()).unwrap());
    let old = Arc::new(ArtifactEntry::original(producer.sha256(), historical.clone()).unwrap());
    assert_ne!(entry.descriptor.id, old.descriptor.id);
    let target = Arc::new(ArtifactEntry::canonical(
        fallback.module_interface().unwrap().clone(),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone(), target.clone()])
        .unwrap();
    let projection =
        CompilerInputProjection::from_issued_entries(&[entry.clone(), target.clone()]).unwrap();
    let source = ExactDeclarationContext::from_authenticated_execution(
        producer.sha256(),
        &view,
        projection,
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![entry.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: entry.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            entry.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    let old_input = OriginalCompilerInputs::from_native_availability(
        &type_context(std::slice::from_ref(&historical)),
        producer,
        std::slice::from_ref(&historical),
    )
    .unwrap();
    assert!(old_input
        .native_offers()
        .iter()
        .any(|offer| offer.is_full()));
    let publication = source
        .issue_published_source_original("revision", "input", &entry.descriptor.owner)
        .unwrap();
    let published_with_fallback = publication
        .context()
        .as_ref()
        .clone()
        .extend_interface_context(&type_context(std::slice::from_ref(&fallback)))
        .expect("publication narrows to B; the type owner explicitly admits unrelated C afterward");
    for published in [false, true] {
        let baseline = if published {
            &published_with_fallback
        } else {
            &source
        };
        let before = baseline.artifact_view().capture_graph_selection();
        let mixed = baseline
            .clone()
            .retain_issued_native_offers(old_input.native_offers().iter().cloned())
            .unwrap();
        assert_eq!(
            mixed.artifact_view().capture_graph_selection().roots,
            before.roots
        );
        assert_eq!(
            mixed.artifact_view().capture_graph_selection().namespace,
            before.namespace
        );
        let roles = mixed.compiler_input_projection().snapshot_roles();
        assert_eq!(
            roles[&entry.descriptor.owner].original(),
            Some(entry.descriptor.id)
        );
        assert!(roles[&target.descriptor.owner].original().is_none());
        assert!(mixed
            .compiler_source_support()
            .native_offers()
            .iter()
            .any(|offer| offer.original() == old.descriptor.id));
        let reused = OriginalCompilerInputs::reuse_selected_authored(&mixed).expect(
            "unrelated InterfaceOnly fallback cannot requalify selected B against historical A",
        );
        assert!(
            reused.is_none(),
            "C has no issued original; B is a source original, not an authored fallback root"
        );
        assert_eq!(
            mixed.artifact_view().capture_graph_selection().roots,
            before.roots
        );
        assert_eq!(
            mixed.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner].original(),
            Some(entry.descriptor.id)
        );
    }
}

#[test]
fn fresh_candidate_dependency_uses_issued_availability_without_selecting_execution() {
    use crate::artifact_inventory::{
        ArtifactBindingNode, ExactArtifactSelection, IssuedNativeOffer, NativeOriginalRequirement,
    };
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let dependency = fixture_finalized_product(
        original_groups_fixture_with_interface(
            "CandidateDependency",
            vec![(3, vec![]), (7, vec![])],
            161,
            &BTreeMap::new(),
            b"candidate dependency interface".to_vec(),
        ),
        producer.sha256(),
    );
    let fresh = dependent_product(
        "FreshDependingOnAvailableGroup",
        162,
        vec![(11, vec![source_import(&dependency, 3)])],
    );
    let products = recovered_witness_fixtures(&[dependency, fresh]);
    let dependency = products[0].product.clone();
    let fresh = products[1].product.clone();
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), dependency.clone()).unwrap());
    let output = OriginalCompilerInputs::from_checked_support_products(
        &crate::artifact_inventory::CompilerSourceSupport::empty(),
        producer.sha256(),
        std::slice::from_ref(&dependency),
    )
    .unwrap();
    let support =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&output).unwrap();
    let issued = support.artifacts().clone();
    let inventory = issued.inventory().clone();
    let full = support
        .native_offers()
        .iter()
        .find(|offer| offer.original() == entry.descriptor.id)
        .unwrap()
        .clone();
    assert!(full.is_full());
    assert_eq!(
        full.issued_group_carriers()
            .unwrap()
            .into_keys()
            .collect::<BTreeSet<_>>(),
        issued
            .capture_graph_selection()
            .native_groups
            .into_iter()
            .collect(),
        "checked offer membership must match the independent eager issued view",
    );
    assert!(full
        .issued_group_carriers()
        .unwrap()
        .keys()
        .any(
            |group| group.binding.artifact() == entry.descriptor.id && group.original_ordinal == 3
        ));
    assert!(support.artifacts().selected_native_groups().is_empty());
    let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer: producer.sha256(),
        compiler_support: support,
        lexical: Default::default(),
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    });
    assert_eq!(
        context.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner].original(),
        Some(entry.descriptor.id)
    );
    assert!(context.artifact_view().selected_native_groups().is_empty());
    assert_eq!(
        context.compiler_source_support().native_offers()[0].snapshot_facts(),
        full.snapshot_facts()
    );
    let before = context.artifact_view().capture_graph_selection();
    let incomplete_recorded_support = recover_fixture_compiler_support(
        context.compiler_input_projection().clone(),
        context.artifact_view().clone(),
        [full.clone()],
    )
    .unwrap();
    let incomplete_recorded_context = context.clone().rebuild(|facts| {
        facts.compiler_support = incomplete_recorded_support;
    });
    let missing_rows = restore_fixture_support_in_view(
        &incomplete_recorded_context,
        context.artifact_view().clone(),
    );
    assert!(
        matches!(&missing_rows, Err(CompileError::ExtractFailed(detail))
        if detail == "exact declaration context: selected original lacks its normalized native permission row"),
        "an incomplete recorded owner cannot enter the valid fixture workflow: {missing_rows:?}"
    );
    let fresh_id = ArtifactEntry::original(producer.sha256(), fresh.clone())
        .unwrap()
        .descriptor
        .id;
    let candidates = crate::module_candidates::CandidateSet {
        manifest_path: PathBuf::from("candidate-native-issuance-only"),
        by_owner: BTreeMap::new(),
        native_availability: vec![fresh],
        catalog_owner: None,
        input_transport: Vec::new(),
    };
    let inputs = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
        .expect("fresh exact body dependency may consume issued group availability without selecting execution")
        .unwrap();
    assert!(inputs
        .issued_artifacts()
        .selected_native_groups()
        .is_empty());
    assert_eq!(context.artifact_view().capture_graph_selection(), before);
    assert!(inputs
        .native_offers()
        .iter()
        .any(|offer| offer.is_full() && offer.original() == fresh_id));

    // A continued input carries its issued row permissions. It has no new
    // product receipt and cannot authorize a fresh body alongside those rows.
    let continue_request = |context: &ExactDeclarationContext| -> Result<_, CompileError> {
        let request = ProtectedScaffoldRequirements::default().compiler_inputs(context, None)?;
        let validation = PackageInterfaceValidation::default();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            &request,
            &request.metadata,
            &validation.inventory,
        )
        .map_err(compiler_evidence_failure)?;
        assert!(selection
            .complete_native_products(request.artifacts())
            .map_err(compiler_evidence_failure)?
            .is_empty());
        let continued = OriginalCompilerInputs::from_selection(&selection, request.artifacts())?;
        assert!(!continued
            .native_offers()
            .iter()
            .any(|offer| offer.original() == fresh_id));
        let choices = selection
            .native_group_candidates(
                crate::artifact_inventory::NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: 3,
                },
                dependency.owner(),
            )
            .map_err(compiler_evidence_failure)?;
        Ok((continued, choices.bindings().clone()))
    };
    let mut carrier_graph = issued.capture_graph_selection();
    carrier_graph
        .roots
        .retain(|node| matches!(node, ArtifactBindingNode::Artifact(_)));
    carrier_graph
        .custody_roots
        .retain(|node| matches!(node, ArtifactBindingNode::Artifact(_)));
    carrier_graph.native_groups.clear();
    let carriers = issued
        .select_issued(
            carrier_graph
                .namespace
                .iter()
                .map(|binding| binding.artifact)
                .collect(),
            &ExactArtifactSelection::from_recovered_graph_selection(carrier_graph),
        )
        .unwrap();
    assert!(carriers.selected_native_groups().is_empty());
    assert!(
        full.qualified_private_availability(context.compiler_input_projection(), &carriers)
            .is_some(),
        "the exact original canonical binding still qualifies the held Full owner"
    );
    assert!(
        full.proof()
            .unwrap()
            .qualify_availability(&carriers)
            .is_err(),
        "the selected carrier view cannot reissue the historical Full proof"
    );
    let carrier_support = restore_fixture_support_in_view(&context, carriers).unwrap();
    assert_eq!(
        carrier_support.recorded_input_decisions(),
        context.compiler_source_support().recorded_input_decisions()
    );
    let carried_full = carrier_support
        .native_offers()
        .iter()
        .find(|offer| offer.original() == entry.descriptor.id)
        .unwrap()
        .clone();
    assert_eq!(carried_full.snapshot_facts(), full.snapshot_facts());
    let carrier_context = context
        .clone()
        .rebuild(|facts| facts.compiler_support = carrier_support);
    let before_carriers = carrier_context.artifact_view().capture_graph_selection();
    let (continued, choices) = continue_request(&carrier_context)
        .expect("the same request-owned Full remains valid beside carrier-only selection");
    assert!(!choices.is_empty());
    assert!(continued
        .issued_artifacts()
        .selected_native_groups()
        .is_empty());
    assert_eq!(
        carrier_context.artifact_view().capture_graph_selection(),
        before_carriers
    );
    assert!(continued
        .native_offers()
        .iter()
        .any(|offer| offer.same_issuance(&carried_full)));

    let (continued, choices) =
        continue_request(&context).expect("request-owned Full keeps its exact available groups");
    assert!(!choices.is_empty());
    assert!(continued
        .issued_artifacts()
        .selected_native_groups()
        .is_empty());
    assert!(continued
        .native_offers()
        .iter()
        .any(|offer| offer.same_issuance(&full)));

    for (ordinals, expected) in [(vec![3], true), (vec![7], false), (vec![], false)] {
        let selected = ordinals
            .iter()
            .map(|ordinal| crate::artifact_inventory::NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            })
            .collect::<BTreeSet<_>>();
        let bounded = inventory
            .admit_recovery_selection(&inventory.empty_view(), vec![entry.clone()], &selected)
            .unwrap();
        let bounded_support =
            fixture_compiler_support(context.compiler_input_projection().clone(), bounded.clone());
        let offer = bounded_support
            .native_offers()
            .iter()
            .find(|offer| offer.original() == entry.descriptor.id)
            .unwrap()
            .clone();
        assert!(!offer.is_full());
        assert_eq!(
            offer
                .issued_group_carriers()
                .unwrap()
                .keys()
                .filter(|group| group.binding.artifact() == entry.descriptor.id)
                .map(|group| group.original_ordinal)
                .collect::<BTreeSet<_>>(),
            ordinals.iter().copied().collect()
        );
        let mut graph = bounded.capture_graph_selection();
        let roots = graph
            .roots
            .iter()
            .filter(|node| matches!(node, ArtifactBindingNode::Group(_)))
            .copied()
            .collect::<Vec<_>>();
        graph
            .roots
            .retain(|node| !matches!(node, ArtifactBindingNode::Group(_)));
        graph.custody_roots.extend(roots);
        graph.custody_roots.sort();
        graph.custody_roots.dedup();
        let namespace = graph
            .namespace
            .iter()
            .map(|binding| binding.artifact)
            .collect();
        let withdrawn = bounded
            .select_issued(
                namespace,
                &ExactArtifactSelection::from_recovered_graph_selection(graph),
            )
            .unwrap();
        let bounded_issuer = context.clone().rebuild(|facts| {
            facts.compiler_support = bounded_support;
        });
        let support = restore_fixture_support_in_view(&bounded_issuer, withdrawn).unwrap();
        assert_eq!(
            support.recorded_input_decisions(),
            bounded_issuer
                .compiler_source_support()
                .recorded_input_decisions(),
            "Partial and explicit Empty keep their exact issued decision domain"
        );
        let restored_offer = support
            .native_offers()
            .iter()
            .find(|candidate| candidate.original() == entry.descriptor.id)
            .unwrap()
            .clone();
        assert_eq!(restored_offer.snapshot_facts(), offer.snapshot_facts());
        let bounded_context = bounded_issuer.rebuild(|facts| facts.compiler_support = support);
        assert!(bounded_context
            .artifact_view()
            .selected_native_groups()
            .is_empty());
        assert_eq!(
            bounded_context.compiler_source_support().native_offers()[0].snapshot_facts(),
            offer.snapshot_facts()
        );
        assert_eq!(
            entry
                .native_group_ordinals
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([3, 7]),
            "physical body census stays complete even when issued availability is bounded"
        );
        let (continued, choices) = continue_request(&bounded_context).unwrap();
        assert_eq!(
            !choices.is_empty(),
            expected,
            "the actual request row independently permits group3, never its full physical census"
        );
        assert!(continued
            .issued_artifacts()
            .selected_native_groups()
            .is_empty());
        assert!(continued
            .native_offers()
            .iter()
            .any(|retained| retained.same_issuance(&restored_offer)));
        let result =
            OriginalCompilerInputs::from_candidates(&bounded_context, producer, &candidates);
        assert_eq!(
            result.is_ok(),
            expected,
            "only an exact issued group3 can support this dependency"
        );
        if expected {
            let inputs = result
                .expect("the exact dependency group is available")
                .expect("fresh candidate issuance must produce a fragment");
            assert!(inputs
                .issued_artifacts()
                .selected_native_groups()
                .is_empty());
            assert!(inputs
                .native_offers()
                .iter()
                .any(|offer| offer.is_full() && offer.original() == fresh_id));
        } else {
            assert!(
                matches!(result, Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact, original_ordinal: 3,
                } if artifact == entry.descriptor.id)),
                "missing exact dependency availability must refuse at its typed owner"
            );
        }
    }
    let historical_view = inventory
        .admit_shared(
            &inventory.empty_view(),
            vec![
                entry.clone(),
                Arc::new(ArtifactEntry::canonical(
                    product("HistoricalDependencyMarker", 163)
                        .module_interface()
                        .unwrap()
                        .clone(),
                )),
            ],
        )
        .unwrap();
    let historical_proof =
        NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &historical_view)
            .unwrap()
            .unwrap();
    let historical = IssuedNativeOffer::issue(
        entry.descriptor.id,
        &historical_view,
        Some(historical_proof),
    )
    .unwrap();
    assert!(historical.is_full());
    let group3 = |offer: &IssuedNativeOffer| {
        *offer
            .issued_group_carriers()
            .unwrap()
            .keys()
            .find(|group| {
                group.binding.artifact() == entry.descriptor.id && group.original_ordinal == 3
            })
            .unwrap()
    };
    let current_group = group3(&full);
    assert_ne!(
        current_group,
        group3(&historical),
        "independent scopes genuinely issue different group bindings"
    );
    for offers in [
        vec![full.clone(), historical.clone()],
        vec![historical.clone(), full.clone()],
        vec![full.clone(), full.clone()],
    ] {
        let support = context
            .compiler_source_support()
            .retain_offers(offers)
            .unwrap();
        let scoped = context
            .clone()
            .rebuild(|facts| facts.compiler_support = support);
        assert_eq!(
            scoped.artifact_view().capture_graph_selection().namespace,
            before.namespace
        );
        assert!(scoped.artifact_view().selected_native_groups().is_empty());
        let inputs = OriginalCompilerInputs::from_candidates(&scoped, producer, &candidates)
            .unwrap()
            .unwrap();
        let fresh_offer = inputs
            .native_offers()
            .iter()
            .find(|offer| offer.original() == fresh_id)
            .unwrap();
        assert!(fresh_offer.is_full());
        let groups = fresh_offer.issued_group_carriers().unwrap();
        let dependent = *groups
            .keys()
            .find(|group| group.binding.artifact() == fresh_id && group.original_ordinal == 11)
            .unwrap();
        let expected = (
            ArtifactBindingNode::Group(dependent),
            ArtifactBindingNode::Group(current_group),
            crate::artifact_inventory::ArtifactDependency::NativeGroup {
                dependent_ordinal: 11,
                required_ordinal: 3,
            },
        );
        assert!(
            fresh_offer
                .custody_snapshot()
                .binding_dependencies()
                .unwrap()
                .contains(&expected),
            "the fresh body uses the exact current carrier's issued dependency edge"
        );
        assert!(inputs
            .issued_artifacts()
            .selected_native_groups()
            .is_empty());
    }
    let historical_support =
        fixture_compiler_support(context.compiler_input_projection().clone(), historical_view);
    let historical_context = context.clone().rebuild(|facts| {
        facts.compiler_support = historical_support;
    });
    let mismatched_issuer = context
        .compiler_source_support()
        .restore_snapshot_evidence(&historical_context.snapshot());
    assert!(matches!(&mismatched_issuer, Err(CompileError::ExtractFailed(detail))
        if detail == "artifact inventory: recovered native offer scope is outside acquisition custody"),
        "a different historical issuer cannot restore its authority into current custody: {mismatched_issuer:?}");
    assert_eq!(
        context.artifact_view().capture_graph_selection().namespace,
        before.namespace
    );
}

#[test]
fn recovery_interface_materialization_work_is_independent_of_native_body_growth() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let mut observed = Vec::new();
    let mut native_sizes = Vec::new();
    for groups in [1u32, 64] {
        let product = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "InspectionGrowth",
                (0..groups).map(|ordinal| (ordinal, vec![])).collect(),
                61,
                &BTreeMap::new(),
                b"fixed interface".to_vec(),
            ),
            producer,
        );
        native_sizes.push(product.product_bytes().len());
        let entry = Arc::new(ArtifactEntry::original(producer, product).unwrap());
        let directory = tempfile::tempdir().unwrap();
        let mut validation = PackageInterfaceValidation::default();
        let files = recovery_artifacts::materialize_selected_interface(
            directory.path(),
            &entry.descriptor.owner,
            entry.clone(),
            producer,
            &mut validation,
        )
        .unwrap();
        let paths = std::fs::read_dir(directory.path().join("artifacts"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            paths.len(),
            2,
            "only interface and package witness are materialized"
        );
        assert!(paths
            .iter()
            .all(|path| path.to_str().unwrap().ends_with(".hi")
                || path.to_str().unwrap().ends_with(".hi.packages")));
        assert!(directory.path().join(&files.interface_path).is_file());
        observed.push(validation.work());
    }
    assert!(
        native_sizes[1] > native_sizes[0],
        "the control must grow native bytes"
    );
    assert_eq!(
        observed[0], observed[1],
        "growing authenticated native bodies must add no copied, read, decoded or hashed bytes"
    );
}

fn native_context(product: &CertifiedRecoveryProduct) -> Arc<ExactDeclarationContext> {
    let inventory = ArtifactInventory::default();
    let entry = Arc::new(
        ArtifactEntry::original(
            CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
            product.clone(),
        )
        .unwrap(),
    );
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let view = inventory
        .admit_shared(&inventory.empty_view(), vec![entry])
        .unwrap();
    Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
            compiler_support: fixture_compiler_support(projection, view),
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ))
}

#[test]
fn recovery_interface_projection_preserves_native_availability_modes() {
    let baseline = product("InspectBaseline", 31);
    let required = product("InspectRequired", 32);
    let unused = product("InspectUnused", 33);
    let full = native_context(&baseline);
    assert!(full
        .compiler_source_support()
        .native_offers()
        .iter()
        .any(|offer| offer.is_full()));
    for context in [
        type_context(&[baseline.clone(), required.clone(), unused.clone()]),
        partial_context(&baseline, &required, &unused, None),
        full,
    ] {
        let before = context.snapshot();
        let inputs = context.recovery_inspection_inputs().unwrap();
        assert!(!inputs.0.artifacts.is_empty());
        assert!(inputs
            .0
            .artifacts
            .iter()
            .all(|artifact| artifact.product.is_none()));
        inputs.0.validate().unwrap();
        let after = context.snapshot();
        assert_eq!(after.producer, before.producer);
        assert_eq!(
            after.graph_selection().unwrap(),
            before.graph_selection().unwrap()
        );
        assert_eq!(after.roles, before.roles);
        assert_eq!(
            after.offers, before.offers,
            "interface inspection cannot issue native availability"
        );
        assert_eq!(after.lexical, before.lexical);
    }
}

#[test]
fn recovery_inspection_ignores_unrelated_retained_native_inventory_growth() {
    let baseline = native_context(&product("InspectedOwner", 48));
    let before = baseline.recovery_inspection_inputs().unwrap();
    let incoming = baseline
        .artifact_view()
        .inventory()
        .admit_shared(
            baseline.artifact_view(),
            vec![Arc::new(
                ArtifactEntry::original(
                    CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
                    product("UnrelatedNativeOwner", 49),
                )
                .unwrap(),
            )],
        )
        .unwrap();
    let support = baseline
        .compiler_source_support()
        .retain_custody(&incoming)
        .unwrap();
    let expanded = Arc::new(
        baseline
            .as_ref()
            .clone()
            .rebuild(|facts| facts.compiler_support = support),
    );
    assert!(
        expanded.artifact_view().artifact_ids().len()
            > baseline.artifact_view().artifact_ids().len(),
        "native custody must actually grow"
    );
    let after = expanded.recovery_inspection_inputs().unwrap();
    let roster = |inputs: &RecoveryInspectionInputs| {
        inputs
            .0
            .artifacts
            .iter()
            .map(|artifact| {
                (
                    artifact.interface.unit.clone(),
                    artifact.interface.module.clone(),
                    artifact.interface.sha256.clone(),
                    artifact.interface.requirements.clone(),
                    artifact.product.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(roster(&before), roster(&after));
    let files = |inputs: &RecoveryInspectionInputs| {
        std::fs::read_dir(inputs.0.directory.path().join("artifacts"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        files(&before),
        files(&after),
        "unrelated native custody must add no copied inspection files or native payload"
    );
}

#[test]
fn retained_template_capture_preserves_normalized_input_modes_with_broader_proof_custody() {
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    let product = product("TemplatePermissionOwner", 61);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let entry = Arc::new(ArtifactEntry::original(producer, product).unwrap());
    let owner = entry.descriptor.owner.clone();
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let full_support = fixture_compiler_support(projection.clone(), full.clone());
    assert!(full_support
        .native_offers()
        .iter()
        .any(|offer| offer.is_full()));
    for ordinals in [vec![7, 9001], vec![7], vec![9001], vec![]] {
        let selected = ordinals
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            })
            .collect();
        let chosen = inventory
            .admit_recovery_selection(&inventory.empty_view(), vec![entry.clone()], &selected)
            .unwrap();
        let support = fixture_compiler_support(projection.clone(), chosen)
            .retain_offers(full_support.native_offers().iter().cloned())
            .unwrap();
        assert!(support.native_offers().iter().any(|offer| offer.is_full()));
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
        let captured =
            RetainedTemplateImports::capture(&original, &[format!("import {}\n", owner.module)])
                .unwrap()
                .unwrap();
        assert!(captured
            .compiler_support
            .projection()
            .roles()
            .iter()
            .all(|role| role.original().is_none()));
        assert!(captured
            .compiler_support
            .artifacts()
            .selected_native_groups()
            .is_empty());
        let exported = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&captured.compiler_support)
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(exported.clone()))
            .prepare_compilation(root.path(), PRODUCER)
            .unwrap();
        let row = request
            .compiler_inputs()
            .native_availability(entry.descriptor.id)
            .expect("explicit empty remains an issued row");
        assert_eq!(row.mode == NativeTransportMode::Full, ordinals.len() == 2);
        assert_eq!(row.permits(7), ordinals.contains(&7));
        assert_eq!(row.permits(9001), ordinals.contains(&9001));
        let snapshot = exported.snapshot();
        let store = MemoryBlockstore::default();
        let mut writer = Materializer::new(&store);
        let committed = writer.materialize_context(None, &snapshot).unwrap();
        for (cid, bytes) in writer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let acquired = hydrate_context(&store, committed.root()).unwrap();
        let view = exported.artifact_view();
        let graph = view.capture_graph_selection();
        let recovered = RecoveredArtifactInventory {
            producer,
            entries: view
                .entries()
                .into_iter()
                .map(|entry| (entry.descriptor.id, entry))
                .collect(),
            recorded_inventory: true,
            bindings: graph.bindings.iter().copied().collect(),
            binding_edges: view.binding_dependencies(),
            interfaces: view.interface_dependencies(),
        };
        let reopened = recovered
            .context_for_expected_snapshot(committed.root(), &acquired)
            .unwrap();
        assert_eq!(
            reopened.snapshot().native_availability,
            snapshot.native_availability
        );
        if ordinals.len() == 2 {
            // The real writer/acquired-root boundary can retain a narrower
            // grant with exactly the same payloads, proofs, roles and demand.
            let mut limited = snapshot.clone();
            limited
                .native_availability
                .get_mut(&entry.descriptor.id)
                .unwrap()
                .mode = NativeTransportMode::IssuedGroups;
            let mut writer = Materializer::new(&store);
            let limited_root = writer.materialize_context(None, &limited).unwrap();
            for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                store.put_keyed(&cid, &bytes).unwrap();
            }
            let limited_receipt = hydrate_context(&store, limited_root.root()).unwrap();
            let limited_context = recovered
                .context_for_expected_snapshot(limited_root.root(), &limited_receipt)
                .unwrap();
            assert_eq!(
                limited_context.snapshot().selection,
                reopened.snapshot().selection
            );
            assert_eq!(limited_context.snapshot().roles, reopened.snapshot().roles);
            assert_eq!(
                limited_context.snapshot().offers,
                reopened.snapshot().offers
            );
            assert_eq!(
                limited_context.snapshot().input_decisions,
                reopened.snapshot().input_decisions
            );
            assert_ne!(
                limited_context.semantic_sha256(),
                reopened.semantic_sha256()
            );
            assert_eq!(
                limited_context.original_semantic_bytes_oracle(
                    &limited_context.artifact_view().metadata_snapshot()
                ),
                limited_context.semantic_bytes_from_metadata(
                    &limited_context.artifact_view().metadata_snapshot()
                )
            );
        }
        if ordinals.len() != 2 {
            let mut widened = snapshot.clone();
            widened
                .native_availability
                .get_mut(&entry.descriptor.id)
                .unwrap()
                .mode = NativeTransportMode::Full;
            let mut writer = Materializer::new(&store);
            let substituted = writer.materialize_context(None, &widened).unwrap();
            for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                store.put_keyed(&cid, &bytes).unwrap();
            }
            let substituted = hydrate_context(&store, substituted.root()).unwrap();
            assert!(
                matches!(recovered.context_for_expected_snapshot(committed.root(), &substituted), Err(CompileError::CompilerEvidence(error)) if matches!(*error, crate::certified_products::CertificationError::Mismatch("acquired compiler snapshot root differs from expected context")))
            );
        }
    }
}

#[test]
fn fresh_fragment_does_not_issue_empty_permissions_for_baseline_originals() {
    let baseline_product = product("BaselineNativePermission", 93);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), baseline_product.clone()).unwrap());
    let owner = entry.descriptor.owner.clone();
    let fresh = crate::certified_products::fixture_finalized_product_with_requirements(
        original_groups_fixture_with_interface(
            "FreshOnlyPermission",
            vec![(3, vec![])],
            94,
            &BTreeMap::new(),
            b"fresh-only canonical".to_vec(),
        ),
        producer.sha256(),
        Some(BTreeMap::from([(
            (owner.unit.clone(), owner.module.clone()),
            baseline_product
                .module_interface()
                .unwrap()
                .interface_sha256(),
        )])),
    );
    let fresh = recovered_witness_fixtures(&[fresh]).remove(0).product;
    let fresh_id = ArtifactEntry::original(producer.sha256(), fresh.clone())
        .unwrap()
        .descriptor
        .id;
    for ordinals in [vec![7, 9001], vec![7], vec![]] {
        let inventory = ArtifactInventory::default();
        let selected = ordinals
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            })
            .collect();
        let view = inventory
            .admit_recovery_selection(&inventory.empty_view(), vec![entry.clone()], &selected)
            .unwrap();
        let baseline = Arc::new(
            ExactDeclarationContext::from_authenticated_execution(
                producer.sha256(),
                &view,
                CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
                vec![ExactLexicalNode {
                    owner: owner.clone(),
                    imports: vec![],
                }],
                owner.clone(),
                std::slice::from_ref(&owner),
            )
            .unwrap(),
        );
        let fragment = OriginalCompilerInputs::from_native_availability(
            &baseline,
            producer,
            std::slice::from_ref(&fresh),
        )
        .unwrap();
        assert_eq!(fragment.role_projection().snapshot_roles().len(), 1);
        assert_eq!(
            fragment
                .native_availability
                .rows()
                .map(|row| row.original.descriptor.id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([fresh_id])
        );
        let scratch = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(baseline)
            .prepare_compilation_with_private_input(scratch.path(), PRODUCER, None, Some(fragment))
            .unwrap();
        let inherited = request
            .compiler_inputs()
            .native_availability(entry.descriptor.id)
            .expect("baseline explicit Empty remains issued by its input owner");
        assert_eq!(
            inherited.mode == NativeTransportMode::Full,
            ordinals.len() == 2
        );
        assert_eq!(inherited.permits(7), ordinals.contains(&7));
        assert_eq!(inherited.permits(9001), ordinals.contains(&9001));
    }
}

// Replay candidate issuance with the same normalized input permission
// and a fresh native edge. Extra selected roots are prior work, not
// obligations of the fresh fragment.
fn replay_fresh_fragment_obligations(
    permitted: &[u32],
    inherited: &[u32],
    siblings: usize,
    grant_full: bool,
) -> BTreeSet<u32> {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let dependency = dependent_product(
        "ObligationBaseline",
        95,
        vec![(7, vec![]), (9001, vec![]), (42, vec![]), (43, vec![])],
    );
    let fresh = dependent_product(
        "ObligationFresh",
        96,
        vec![(3, vec![source_import(&dependency, 7)])],
    );
    let mut products = vec![dependency, fresh];
    products.extend((0..siblings).map(|index| product(&format!("ObligationSibling{index}"), 97)));
    let products = recovered_witness_fixtures(&products);
    let dependency = &products[0].product;
    let fresh = &products[1].product;
    let input = Arc::new(ArtifactEntry::original(producer.sha256(), dependency.clone()).unwrap());
    let fresh_id = ArtifactEntry::original(producer.sha256(), fresh.clone())
        .unwrap()
        .descriptor
        .id;
    let mut entries = vec![input.clone()];
    entries.extend(products[2..].iter().map(|row| {
        Arc::new(ArtifactEntry::original(producer.sha256(), row.product.clone()).unwrap())
    }));
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), entries.clone())
        .unwrap();
    let full_support = fixture_compiler_support(projection.clone(), full);
    let selected = inherited
        .iter()
        .map(|ordinal| NativeGroupKey {
            artifact: input.descriptor.id,
            original_ordinal: *ordinal,
        })
        .chain(entries[1..].iter().map(|entry| NativeGroupKey {
            artifact: entry.descriptor.id,
            original_ordinal: 9001,
        }))
        .collect::<BTreeSet<_>>();
    let chosen = if grant_full {
        // Change demand under this same issued carrier and canonical namespace.
        // A separate admission would issue different bindings and correctly
        // demote the earlier Full qualification during normalization.
        let issued = full_support.artifacts();
        let mut graph = issued.capture_graph_selection();
        let namespace = graph.namespace.clone();
        let unused = graph
            .roots
            .iter()
            .filter(|node| match node {
                ArtifactBindingNode::Group(group) => !selected.contains(&NativeGroupKey {
                    artifact: group.binding.artifact(),
                    original_ordinal: group.original_ordinal,
                }),
                ArtifactBindingNode::Artifact(_) => false,
            })
            .copied()
            .collect::<Vec<_>>();
        graph.roots.retain(|node| !unused.contains(node));
        graph.custody_roots.extend(unused);
        graph.custody_roots.sort();
        graph.custody_roots.dedup();
        let chosen = issued
            .select_issued(
                namespace.iter().map(|binding| binding.artifact).collect(),
                &crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(
                    graph,
                ),
            )
            .unwrap();
        assert_eq!(chosen.capture_graph_selection().namespace, namespace);
        assert_eq!(chosen.selected_native_groups(), selected);
        chosen
    } else {
        inventory
            .admit_recovery_selection(&inventory.empty_view(), entries, &selected)
            .unwrap()
    };
    let mut support = fixture_compiler_support(projection, chosen)
        .retain_offers(full_support.native_offers().iter().cloned())
        .unwrap();
    if grant_full {
        support = support
            .retain_normalized_availability(full_support.native_availability())
            .unwrap();
    }
    let input_row = support
        .native_availability()
        .rows()
        .find(|row| row.original.descriptor.id == input.descriptor.id)
        .unwrap();
    assert!(
        support
            .native_offers()
            .iter()
            .any(|offer| { offer.original() == input.descriptor.id && offer.is_full() }),
        "all cases retain genuine broader Full proof custody"
    );
    assert_eq!(input_row.mode == NativeTransportMode::Full, grant_full);
    for ordinal in [7, 9001, 42, 43] {
        assert_eq!(input_row.permits(ordinal), permitted.contains(&ordinal));
    }
    let owner = input.descriptor.owner.clone();
    let context = Arc::new(
        ExactDeclarationContext::from_authenticated_execution_support(
            producer.sha256(),
            support,
            vec![ExactLexicalNode {
                owner: owner.clone(),
                imports: vec![],
            }],
            owner,
            &[],
        )
        .unwrap(),
    );
    let source_before = context.artifact_view().capture_graph_selection();
    let candidate = OriginalCompilerInputs::from_native_availability(
        &context,
        producer,
        std::slice::from_ref(fresh),
    );
    let allowed = permitted.contains(&7);
    let consumer = "candidate";
    let result = candidate;
    {
        if !allowed {
            let actual_error = format!("{:?}", result.as_ref().err());
            assert!(
                matches!(result, Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact, original_ordinal: 7,
                } if artifact == input.descriptor.id)),
                "{consumer}: broader proof cannot grant unavailable B7; actual_error={actual_error}"
            );
            assert_eq!(
                context.artifact_view().capture_graph_selection(),
                source_before
            );
            return BTreeSet::new();
        }
        let fragment = result.unwrap_or_else(|error| {
            panic!("{consumer}: inherited={inherited:?} siblings={siblings}: {error:?}")
        });
        let fresh_row = fragment
            .native_availability()
            .rows()
            .find(|row| row.original.descriptor.id == fresh_id)
            .unwrap();
        assert_eq!(fresh_row.mode, NativeTransportMode::Full);
        let fresh_groups = fresh_row.issued_ordinals();
        assert_eq!(fresh_groups, BTreeSet::from([3]));
        assert!(
            !fragment
                .issued_artifacts()
                .selected_native_groups()
                .iter()
                .any(|group| { group.artifact == fresh_id }),
            "fresh availability does not select fresh execution"
        );
        let dependency_row = fragment
            .native_availability()
            .rows()
            .find(|row| row.original.descriptor.id == input.descriptor.id)
            .unwrap();
        assert_eq!(dependency_row.mode, NativeTransportMode::IssuedGroups);
        assert_eq!(
            fragment
                .native_availability()
                .rows()
                .map(|row| row.original.descriptor.id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([fresh_id, input.descriptor.id]),
        );
        let expected = BTreeSet::from([7]);
        let actual = [7, 9001, 42, 43]
            .into_iter()
            .filter(|ordinal| dependency_row.permits(*ordinal))
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "{consumer} exact dependency permissions");
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            source_before
        );
        actual
    }
}

#[test]
fn fresh_native_dependency_obligations_exclude_inherited_selected_groups() {
    for permitted in [vec![7, 9001, 42, 43], vec![7], vec![9001], vec![]] {
        replay_fresh_fragment_obligations(&permitted, &permitted, 0, permitted.len() == 4);
    }
}

#[test]
fn whole_selected_input_still_refuses_group_outside_its_issued_availability() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry = Arc::new(
        ArtifactEntry::original(producer.sha256(), product("SelectedObligation", 98)).unwrap(),
    );
    let inventory = ArtifactInventory::default();
    let selected = BTreeSet::from([NativeGroupKey {
        artifact: entry.descriptor.id,
        original_ordinal: 7,
    }]);
    let view = inventory
        .admit_recovery_selection(&inventory.empty_view(), vec![entry.clone()], &selected)
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let support = fixture_compiler_support(projection.clone(), view.clone());
    let mut metadata = projection
        .project_metadata(view.metadata_snapshot())
        .unwrap();
    let unsupported = NativeGroupKey {
        artifact: entry.descriptor.id,
        original_ordinal: 9001,
    };
    Arc::make_mut(&mut metadata.retained)
        .selected_native_groups
        .insert(unsupported);
    let error = NativeCompilerTransport::issue_checked(
        &projection,
        &metadata,
        &view,
        support.native_offers(),
    )
    .err()
    .expect("whole selected ingress must validate all its own selected obligations");
    assert!(matches!(error, CompileError::ExtractFailed(message)
    if message == format!(
        "exact declaration context: selected native group is outside issued availability: stage=whole-selected owner={:?} group={unsupported:?}",
        entry.descriptor.owner,
    )));
}

#[test]
fn generated_fresh_native_obligations_ignore_unrelated_selected_history() {
    use proptest::prelude::*;
    use proptest::test_runner::{contextualize_config, Config, TestCaseError, TestRunner};
    let config = Config {
        cases: 24,
        source_file: Some(file!()),
        test_name: Some(concat!(
            module_path!(),
            "::generated_fresh_native_obligations_ignore_unrelated_selected_history"
        )),
        ..Config::default()
    };
    TestRunner::new(contextualize_config(config))
        .run(
            &(any::<[bool; 3]>(), 0usize..4),
            |(extra_groups, siblings)| {
                let mut inherited = vec![7];
                inherited.extend(
                    [9001, 42, 43]
                        .into_iter()
                        .zip(extra_groups)
                        .filter_map(|(ordinal, selected)| selected.then_some(ordinal)),
                );
                std::panic::catch_unwind(|| {
                    let allowed = [7, 9001, 42, 43];
                    let baseline = replay_fresh_fragment_obligations(&allowed, &[7], 0, true);
                    let varied =
                        replay_fresh_fragment_obligations(&allowed, &inherited, siblings, true);
                    assert_eq!(
                        varied, baseline,
                        "fresh permission must ignore unrelated inherited demand"
                    );
                })
                .map_err(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| {
                            panic
                                .downcast_ref::<&str>()
                                .map(|value| (*value).to_owned())
                        })
                        .unwrap_or_else(|| "non-string panic".into());
                    TestCaseError::fail(format!(
                        "extra_groups={extra_groups:?} siblings={siblings}: {message}"
                    ))
                })
            },
        )
        .unwrap();
}
