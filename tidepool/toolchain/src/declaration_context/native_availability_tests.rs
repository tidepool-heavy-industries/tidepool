use super::*;
use crate::certified_products::tests::{
    original_groups_fixture_with_interface, recovered_witness_fixtures,
};
use crate::certified_products::{
    fixture_finalized_product, fixture_module_core, AcceptedGlobal, AuthenticatedTargetImports,
    ReceiptImportOwner,
};
use tidepool_repr::execution_schema::{GlobalDecl, RuntimeRep};

// Fixtures explicitly issue availability at their input boundary, before any
// compiler request consumes or combines their custody.
fn fixture_compiler_support(
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
) -> crate::artifact_inventory::CompilerSourceSupport {
    crate::declaration_context::issue_fixture_compiler_support(projection, artifacts).unwrap()
}

// Checked normalization issues permissions from these exact offers. Recovery
// authenticates the recorded namespace and custody; raw offers alone grant none.
pub(super) fn fixture_support_with_issued_offers(
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
    offers: impl IntoIterator<Item = crate::artifact_inventory::IssuedNativeOffer>,
) -> Result<crate::artifact_inventory::CompilerSourceSupport, CompileError> {
    let offers = offers.into_iter().collect::<Vec<_>>();
    let recovered =
        recover_fixture_compiler_support(projection, artifacts, offers.iter().cloned())?;
    let availability = RequestNativeAvailability::issue_checked(
        recovered.projection(),
        recovered.artifacts(),
        &offers,
    )?;
    let issued = recovered.retain_normalized_availability(&availability)?;
    issued
        .native_availability()
        .validate_role_coverage(issued.projection())?;
    Ok(issued)
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
        issued
            .compiler_source_support()
            .native_offers()
            .iter()
            .cloned(),
    )?;
    assert!(
        issued
            .compiler_source_support()
            .native_offers()
            .iter()
            .all(|offer| {
                offer
                    .custody_snapshot()
                    .scopes
                    .iter()
                    .all(|(selection, proof)| {
                        recovered
                            .artifacts()
                            .selection_snapshot()
                            .scopes
                            .get(selection)
                            == Some(proof)
                    })
            }),
        "authenticated recovery retains each exact issued offer scope before snapshot restore"
    );
    recovered.restore_snapshot_evidence(
        &issued.snapshot(),
        &BTreeMap::new(),
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
}

const PRODUCER: &[u8] = b"protected-native-availability-fixture";

pub(super) fn product(module: &str, version: u8) -> CertifiedRecoveryProduct {
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
            source_scope: Default::default(),
            template_imports: None,
        },
    ))
}

fn scope(request: &ExactCompilationRequest) -> Vec<Value> {
    let bytes = std::fs::read(&request.manifest).unwrap();
    let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let Value::Array(fields) = value else {
        panic!("scope array");
    };
    assert_eq!(fields.len(), 11);
    assert_eq!(fields[0], text("TPEXACTSCOPE"));
    assert_eq!(fields[1], text("16"));
    fields
}

#[test]
fn production_issued_contribution_preserves_home_owner() {
    let native = product("ContributionHome", 34);
    let owner = ExactModuleIdentity {
        unit: native.owner().unit.clone(),
        module: native.owner().module.clone(),
    };
    let context = native_context(&native);
    let contribution = context
        .compiler_source_support()
        .native_availability()
        .rows()
        .flat_map(|row| row.contributions.iter())
        .next()
        .expect("the production offer issues its request contribution");
    assert!(
        contribution.has_home_original_owner(&owner),
        "an issued Contribution retains its HOME Original owner"
    );
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
            source_scope: Default::default(),
            template_imports: None,
        },
    ))
}

struct NativeWireRow {
    selected_ordinals: Vec<u32>,
    availability: Value,
    classification: Value,
}

fn native_wire_rows(
    request: &ExactCompilationRequest,
) -> BTreeMap<(String, String), NativeWireRow> {
    let fields = scope(request);
    let mut rows = BTreeMap::new();
    for row in fields[6].as_array().expect("native rows") {
        let [unit, module, _, _, _, _, selected, _, availability, classification] =
            row.as_array().expect("native row").as_slice()
        else {
            panic!("native row has ten fields");
        };
        let key = (
            unit.as_text().expect("native unit").to_owned(),
            module.as_text().expect("native module").to_owned(),
        );
        let selected_ordinals = selected
            .as_array()
            .expect("selected native ordinals")
            .iter()
            .map(|ordinal| u32::try_from(ordinal.as_integer().expect("native ordinal")).unwrap())
            .collect();
        match availability
            .as_array()
            .expect("native availability")
            .as_slice()
        {
            [tag] if tag == &text("full-carrier") => {}
            [tag, ordinals] if tag == &text("issued-groups") => {
                for ordinal in ordinals.as_array().expect("available native ordinals") {
                    u32::try_from(ordinal.as_integer().expect("available native ordinal")).unwrap();
                }
            }
            _ => panic!("unknown native availability"),
        }
        assert!(matches!(
            classification.as_text(),
            Some("compiler-original" | "private-native")
        ));
        assert!(
            rows.insert(
                key,
                NativeWireRow {
                    selected_ordinals,
                    availability: availability.clone(),
                    classification: classification.clone(),
                }
            )
            .is_none(),
            "duplicate native wire owner"
        );
    }
    rows
}

fn wire_row(request: &ExactCompilationRequest, module: &str) -> NativeWireRow {
    let mut matches = native_wire_rows(request)
        .into_iter()
        .filter(|((_, name), _)| name == module);
    let (_, row) = matches.next().expect("native wire owner");
    assert!(matches.next().is_none(), "ambiguous native module name");
    row
}

fn wire_groups(request: &ExactCompilationRequest) -> BTreeMap<String, Vec<u32>> {
    let mut groups = BTreeMap::new();
    for ((_, module), row) in native_wire_rows(request) {
        assert!(
            groups.insert(module, row.selected_ordinals).is_none(),
            "ambiguous native module name"
        );
    }
    groups
}

fn wire_mode(request: &ExactCompilationRequest, module: &str) -> Value {
    wire_row(request, module).availability
}

fn wire_classification(request: &ExactCompilationRequest, module: &str) -> Value {
    wire_row(request, module).classification
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
        source_scope: Default::default(),
        template_imports: None,
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
                    &issuer,
                    &metadata,
                    parents,
                    tempfile::tempdir().unwrap(),
                    &RequestInputTransport::OwnedArena,
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
fn selected_output_carries_baseline_type_dependency_without_granting_body() {
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
    let private = {
        let output =
            Arc::new(ArtifactEntry::original(producer.sha256(), consumer.clone()).unwrap());
        let dependency = Arc::new(ArtifactEntry::canonical(
            baseline_product.module_interface().unwrap().clone(),
        ));
        let artifacts = baseline
            .artifact_view()
            .inventory()
            .admit_shared_with_demand(
                baseline.artifact_view(),
                vec![output.clone()],
                crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection =
            CompilerInputProjection::from_issued_entries(&[output, dependency]).unwrap();
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        CapturedCompilerStage::issue(&mut selection, &artifacts, Vec::new(), &mut validation)
            .unwrap()
            .compiler_inputs()
            .as_ref()
            .clone()
    };
    let offered = identity("fixture", "PrivateFragment");
    assert_eq!(
        private
            .projection
            .snapshot_roles()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([offered.clone(), dependency.clone()])
    );
    private.projection.validate(&private.artifacts).unwrap();
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
        .expect("the selected output carries the same closed baseline type evidence");
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
    let availability = final_inputs.native_availability(original).unwrap();
    let final_proof = availability
        .offers()
        .filter(|offer| offer.original() == original)
        .find_map(|offer| offer.proof())
        .unwrap();
    assert_eq!(final_proof.original, original);
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
    let output = Arc::new(ArtifactEntry::original(producer.sha256(), consumer.clone()).unwrap());
    let incomplete = CompilerInputProjection::from_issued_entries(&[output]).unwrap();
    let error = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
        &incomplete,
        &private.artifacts.metadata_snapshot(),
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
    .expect_err("retained type bytes cannot replace the selected required type role");
    assert!(matches!(
        error,
        crate::certified_products::CertificationError::Mismatch("compiler original projection")
    ));
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
    let view = right.retain_custody(&left).unwrap();
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
        .try_fold(view.clone(), |view, partial| view.retain_custody(partial))
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
    let root = tempfile::tempdir().unwrap();
    let close_source = |selected: &ArtifactView, supplied: &[NativeOriginalRequirement]| {
        let offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
            &baseline, selected, supplied,
        )
        .unwrap();
        let support = fixture_support_with_issued_offers(
            baseline.clone(),
            selected.retain_custody(&broad).unwrap(),
            offers,
        )
        .unwrap();
        let source_context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: support,
            lexical: Default::default(),
            source_scope: Default::default(),
            template_imports: None,
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
        crate::certified_products::tests::assert_request_private_product_uses_carried_dependency(
            &source,
            consumer,
            expected,
            PRODUCER,
            &root.path().join(index.to_string()),
        );
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
    // The request selects B7 before broader custody becomes available. The
    // ancestor must reuse that actual binding when it imports B7 later.
    let selected_b = inventory
        .admit_recovery_selection(
            &base,
            vec![entries[1].clone()],
            &BTreeSet::from([NativeGroupKey {
                artifact: entries[1].descriptor.id,
                original_ordinal: 7,
            }]),
        )
        .unwrap();
    let selected_b7 = selected_b.selected_native_binding_rows().1;
    let [selected_b7] = selected_b7.as_slice() else {
        panic!("the input request selects exactly B7");
    };
    let selected_b7 = *selected_b7;
    let full = inventory
        .admit_shared(&selected_b, entries.clone())
        .unwrap();
    let ancestor_group = full
        .selected_native_binding_rows()
        .1
        .into_iter()
        .find(|group| group.binding.artifact() == entries[0].descriptor.id)
        .unwrap();
    assert!(full.binding_dependencies().contains(&(
        ArtifactBindingNode::Group(ancestor_group),
        ArtifactBindingNode::Group(selected_b7),
        ArtifactDependency::NativeGroup {
            dependent_ordinal: 7,
            required_ordinal: 7,
        },
    )));
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
    let transport = NativeInputSelection::issue_checked(
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
    let receipt = transport.availability.0[&entries[1].descriptor.id].clone();
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
    let target_binders = [7, 9001].map(|ordinal| {
        crate::certified_products::original_available_groups(&recovered[1].product)
            .unwrap()
            .find(|group| group.original_ordinal() == ordinal)
            .unwrap()
            .binders()[0]
            .clone()
    });
    let target_owner = recovered[1].product.owner();
    let accepted = [7, 9001]
        .into_iter()
        .zip(&target_binders)
        .map(|(ordinal, binder)| AcceptedGlobal {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Source {
                unit: target_owner.unit.clone(),
                module: target_owner.module.clone(),
                module_version: Some(target_owner.module_version.clone()),
                original_ordinal: ordinal,
                binder: binder.clone(),
            },
        })
        .collect::<Vec<_>>();
    let targets = target_binders
        .iter()
        .map(|binder| {
            prepared_target_fixture(vec![GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            }])
        })
        .collect::<Vec<_>>();
    let mixed_target = prepared_target_fixture(
        target_binders
            .iter()
            .map(|binder| GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            })
            .collect(),
    );
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
    let b = selected_b
        .select_roots(vec![b_id])
        .unwrap()
        .retain_custody(&full.select_roots(vec![b_id]).unwrap())
        .unwrap();
    assert_eq!(b.selected_native_binding_rows().1, [selected_b7]);
    assert!(b
        .capture_graph_selection()
        .native_groups
        .iter()
        .any(|group| group.binding.artifact() == b_id && group.original_ordinal == 9001));
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
            source_scope: Default::default(),
            template_imports: None,
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
    assert_eq!(exact, BTreeSet::from([selected_b7]));
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
    let mut validation = PackageInterfaceValidation::default();
    let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        &inputs,
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
    let private = OriginalCompilerInputs::from_issued_support(inputs.compiler_source_support());
    drop(inputs);
    drop(b_context);
    drop(dependency_offer);
    drop(transport);
    drop(transport_metadata);
    drop(metadata);
    drop(unrelated_offer);
    drop(ancestor_offer);
    drop(full);
    drop(selected_b);
    drop(base);
    drop(entries);
    drop(recovered);
    assert!(
        unrelated_payload.upgrade().is_none(),
        "a B receipt must release unrelated C payload ownership"
    );

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
    assert_eq!(
        graph
            .roots
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(*group),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([selected_b7]),
    );
    assert!(admitted
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: b_id,
            original_ordinal: 7,
        }])
        .expect("the input request's selected B7 remains executable")
        .bindings
        .is_empty());
    let consume = |view: &ArtifactView,
                   selection: &crate::certified_products::CertifiedSourceSelection,
                   target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
                   accepted: &[AcceptedGlobal],
                   validation: &mut PackageInterfaceValidation| {
        let mut selected = selection.clone();
        let physical = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selected,
            view,
            Vec::new(),
            validation,
        )?;
        let observed = AuthenticatedTargetImports::issue(target, accepted, &validation.inventory)
            .map_err(compiler_evidence_failure)?;
        let demanded = view.inventory().select_native_target(
            physical.physical_cohort(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            validation,
        )?;
        demanded.resolve().map(|resolved| resolved.groups().clone())
    };
    let cursor_before = admitted.capture_graph_selection();
    let denied = consume(
        &admitted,
        &selected,
        &mixed_target,
        &accepted,
        &mut validation,
    )
    .unwrap_err();
    assert!(
        matches!(&denied, CompileError::ArtifactInventory(error) if matches!(error.failure,
        crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
            artifact, original_ordinal: 9001,
        } if artifact == b_id)),
        "known B9001 body must refuse as permission, actual={denied:?}"
    );
    let CompileError::ArtifactInventory(ref denied_error) = denied else {
        unreachable!()
    };
    assert!(matches!(
        &denied_error.native_refusal,
        Some(
            crate::artifact_inventory::NativeAdmissionRefusal::InputPermissionDenied {
                body_present: true,
                ..
            } | crate::artifact_inventory::NativeAdmissionRefusal::BoundPermissionDenied {
                body_present: true,
                ..
            }
        )
    ));
    let classified = crate::failclass::classify_compile(&denied);
    assert!(classified.message.contains("PermissionDenied"));
    let cause = serde_json::to_value(classified.cause).unwrap();
    assert!(matches!(
        cause["native_refusal"]["reason"].as_str(),
        Some("input_permission_denied" | "bound_permission_denied")
    ));
    assert_eq!(cause["native_refusal"]["body_present"], true);
    assert_eq!(admitted.capture_graph_selection(), cursor_before);
    let retried = consume(
        &admitted,
        &selected,
        &targets[0],
        &accepted[..1],
        &mut validation,
    )
    .unwrap();
    assert_eq!(
        retried, exact,
        "failed mixed selection cannot change the retry's exact issuer"
    );
    let groups = consume(
        &admitted,
        &selected,
        &targets[0],
        &accepted[..1],
        &mut validation,
    )
    .expect("the issued B receipt authorizes its exact group7");
    assert_eq!(groups, exact);
    assert_eq!(
        groups
            .iter()
            .map(|group| (group.binding.artifact(), group.original_ordinal))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(b_id, 7)])
    );
    // Lifetime inspection can read held B9001 code without selecting it. The
    // target admission below still refuses its unavailable execution permission.
    assert!(admitted
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: b_id,
            original_ordinal: 9001,
        }])
        .expect("B9001 body custody remains independently observable")
        .bindings
        .is_empty());

    assert!(matches!(
        consume(&admitted, &selected, &targets[1], &accepted[1..], &mut validation),
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
            source_scope: Default::default(),
            template_imports: None,
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
    assert!(!request.native_selection.availability.0.contains_key(&a_id));
    let offers = request
        .native_selection
        .availability
        .0
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
    assert_eq!(
        graph
            .roots
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(*group),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([selected_b7]),
    );
    let requested_groups = consume(
        &selected_view,
        &request_selection,
        &targets[0],
        &accepted[..1],
        &mut validation,
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
    assert!(!request.native_selection.availability.0[&b_id].permits(9001));
    assert!(matches!(
        consume(&selected_view, &request_selection, &targets[1], &accepted[1..], &mut validation),
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
        let offer = IssuedNativeOffer::issue(entry.descriptor.id, &source, Some(proof)).unwrap();
        (offer, source)
    };
    let (left, left_source) = issue("OptionalCensusLeft", 103);
    let (right, right_source) = issue("OptionalCensusRight", 104);
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
        let opaque_custody = retain_native_offers(&base, &alternatives).unwrap();
        assert!(opaque_custody
            .capture_graph_selection()
            .native_groups
            .is_empty());
        assert!(NativeOriginalRequirement::from_issued_group_custody(
            entry.descriptor.id,
            canonical,
            &opaque_custody,
        )
        .unwrap()
        .is_none());
        let sources = if reverse {
            [&right_source, &left_source]
        } else {
            [&left_source, &right_source]
        };
        let ambiguous = sources
            .into_iter()
            .try_fold(opaque_custody, |view, source| view.retain_custody(source))
            .unwrap();
        let groups = ambiguous
            .capture_graph_selection()
            .native_groups
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert!(left_groups
            .keys()
            .chain(right_groups.keys())
            .all(|group| groups.contains(group)));
        assert_eq!(
            ambiguous.capture_graph_selection().namespace,
            base.capture_graph_selection().namespace,
        );
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
                source_scope: Default::default(),
                template_imports: None,
            },
        ));
        let baseline = ProtectedScaffoldRequirements::default()
            .compiler_inputs(&context, None)
            .unwrap();
        let empty = &baseline.native_selection.availability.0[&entry.descriptor.id];
        assert_eq!(empty.offers().len(), 1);
        let baseline_empty = empty.offers().next().unwrap().clone();
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
            let artifacts = retain_native_offers(&base, std::slice::from_ref(chosen)).unwrap();
            let support =
                fixture_support_with_issued_offers(projection.clone(), artifacts, [chosen.clone()])
                    .unwrap();
            let private = OriginalCompilerInputs::from_issued_support(&support);
            let root = tempfile::tempdir().unwrap();
            let request = ExactCompileContext::new(context.clone())
                .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(private))
                .unwrap();
            assert_eq!(wire_mode(&request, "OptionalCensus"), full_mode());
            assert!(request.groups.is_empty());
            let availability = request
                .compiler_inputs()
                .native_availability(chosen.original())
                .unwrap();
            assert!(!baseline_empty.same_issuance(chosen));
            assert_eq!(availability.offers().len(), 2);
            assert_eq!(
                availability
                    .offers()
                    .filter(|offer| offer.same_issuance(&baseline_empty))
                    .count(),
                1,
            );
            assert_eq!(
                availability
                    .offers()
                    .filter(|offer| offer.same_issuance(chosen))
                    .count(),
                1,
            );
            let retained = availability
                .offers()
                .find(|offer| offer.same_issuance(chosen))
                .unwrap();
            for ordinal in [7, 9001] {
                let support = availability
                    .dependency_support(
                        crate::artifact_inventory::NativeGroupKey {
                            artifact: entry.descriptor.id,
                            original_ordinal: ordinal,
                        },
                        native.owner(),
                    )
                    .unwrap();
                let expected = chosen
                    .issued_group_carriers()
                    .unwrap()
                    .into_keys()
                    .filter(|group| group.original_ordinal == ordinal)
                    .collect::<BTreeSet<_>>();
                let actual = support.groups().collect::<BTreeSet<_>>();
                assert!(!expected.is_empty());
                assert_eq!(actual, expected);
                for group in actual {
                    assert!(support
                        .offer_for_group(group)
                        .unwrap()
                        .same_issuance(chosen));
                }
            }
            assert!(retained.same_issuance(chosen));
            assert_eq!(retained.proof(), chosen.proof());
            assert_eq!(
                retained
                    .proof()
                    .unwrap()
                    .issued_group_carriers(request.compiler_inputs().artifacts())
                    .unwrap(),
                chosen.issued_group_carriers().unwrap()
            );
        }
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
    let dependency_groups = a_offer
        .dependency_group_carriers(&BTreeSet::from([b_id]))
        .unwrap();
    assert_eq!(
        dependency_groups
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
    let support = fixture_support_with_issued_offers(
        projection.clone(),
        retain_native_offers(&b, &offers).unwrap(),
        offers,
    )
    .unwrap();
    let private = OriginalCompilerInputs::from_issued_support(&support);
    let context = |view, projection| {
        ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: fixture_compiler_support(projection, view),
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            source_scope: Default::default(),
            template_imports: None,
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
        *dependency_groups
            .keys()
            .find(|group| group.binding.artifact() == artifact && group.original_ordinal == ordinal)
            .unwrap()
    };
    // Type-only selection omits executable edges; retained scope proofs own custody.
    assert!(
        typed.selection_snapshot().scopes.values().any(|scope| scope
            .proof()
            .dependencies
            .contains(&(
                ArtifactBindingNode::Group(group(b_id, 7)),
                ArtifactBindingNode::Group(group(a_id, 9001)),
                crate::artifact_inventory::ArtifactDependency::NativeGroup {
                    dependent_ordinal: 7,
                    required_ordinal: 9001,
                },
            ))),
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
        NativeInputSelection::issue_checked(&projection, &metadata, &current, &[offer]).unwrap();
    let support = transport.availability.0[&a_id]
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
fn value_interface_append_keeps_materialized_inputs_until_last_context_release() {
    let native = product("ValueAppendMaterialization", 196);
    let required = identity(&native.owner().unit, &native.owner().module);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&{
                let entry =
                    Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
                let inventory = ArtifactInventory::default();
                let artifacts = inventory
                    .admit_shared_with_demand(
                        &inventory.empty_view(),
                        vec![entry.clone()],
                        crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                    )
                    .unwrap();
                let projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
                let mut validation = PackageInterfaceValidation::default();
                let mut selection =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &projection,
                        &artifacts.metadata_snapshot(),
                        &validation.inventory,
                    )
                    .unwrap();
                let stage = CapturedCompilerStage::issue(
                    &mut selection,
                    &artifacts,
                    Vec::new(),
                    &mut validation,
                )
                .unwrap();
                let output = stage.compiler_inputs();
                crate::artifact_inventory::CompilerSourceSupport::close_complete_output(output)
                    .unwrap()
            })
            .unwrap(),
    );
    let scratch = tempfile::tempdir().unwrap();
    let completed = context
        .prepare_compilation(scratch.path(), PRODUCER)
        .unwrap();
    let directory = completed
        .materialization
        .as_ref()
        .unwrap()
        ._directory
        .as_ref()
        .unwrap();
    let directory_owner = Arc::downgrade(directory);
    let directory_path = directory.path().to_path_buf();
    let interface = crate::certified_products::fixture_module_interface(
        producer.sha256(),
        "fixture",
        "FreshValueMaterialization",
        BTreeMap::new(),
    );
    let value = Arc::new(
        CertifiedValueInterface::from_checked_compilation(
            producer.sha256(),
            identity(interface.unit(), interface.module()),
            interface.interface_bytes().to_vec(),
            interface.package_imports_bytes().to_vec(),
            vec![required],
        )
        .unwrap(),
    );
    let appended = context
        .as_ref()
        .clone()
        .extend_program_value_interface(value)
        .unwrap();
    drop(completed);
    drop(context);
    assert!(directory_owner.upgrade().is_some());
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    drop(appended);
    assert!(directory_owner.upgrade().is_none());
    assert!(!directory_path.try_exists().unwrap());
}

#[test]
fn custody_only_native_offer_keeps_materialized_inputs_after_issuer_release() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let native = product("CustodyMaterialization", 96);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&{
                let entry =
                    Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
                let inventory = ArtifactInventory::default();
                let artifacts = inventory
                    .admit_shared_with_demand(
                        &inventory.empty_view(),
                        vec![entry.clone()],
                        crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                    )
                    .unwrap();
                let projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
                let mut validation = PackageInterfaceValidation::default();
                let mut selection =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &projection,
                        &artifacts.metadata_snapshot(),
                        &validation.inventory,
                    )
                    .unwrap();
                let stage = CapturedCompilerStage::issue(
                    &mut selection,
                    &artifacts,
                    Vec::new(),
                    &mut validation,
                )
                .unwrap();
                let output = stage.compiler_inputs();
                crate::artifact_inventory::CompilerSourceSupport::close_complete_output(output)
                    .unwrap()
            })
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
    let (offer, retained_offers, retained_custody) = {
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
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                inputs,
                &validation.inventory,
            )
            .unwrap();
        let physical = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            inputs.artifacts(),
            Vec::new(),
            &mut validation,
        )
        .unwrap();
        let source = inputs.artifacts();
        let group = crate::certified_products::original_available_groups(product)
            .unwrap()
            .find(|group| group.original_ordinal() == 7)
            .unwrap();
        let binder = group.binders()[0].clone();
        let prepared = prepared_target_fixture(vec![GlobalDecl {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        }]);
        let accepted = [AcceptedGlobal {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Source {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
                module_version: Some(product.owner().module_version.clone()),
                original_ordinal: 7,
                binder,
            },
        }];
        let observed =
            AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory).unwrap();
        let target = source
            .inventory()
            .select_native_target(
                physical.physical_cohort(),
                observed,
                None,
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
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
        let offer =
            IssuedNativeOffer::issue(entry.descriptor.id, target.artifacts(), Some(proof)).unwrap();
        let retained_offers = context
            .compiler_source_support()
            .retain_offers([offer.clone()])
            .unwrap();
        let retained_custody = context
            .compiler_source_support()
            .retain_custody(target.artifacts())
            .unwrap();
        assert!(retained_offers
            .artifacts()
            .selected_native_groups()
            .is_empty());
        assert!(retained_custody
            .artifacts()
            .selected_native_groups()
            .is_empty());
        (offer, retained_offers, retained_custody)
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
        directory_owner.upgrade().is_some(),
        "custody-only support retains the acquired files after the original offer drops"
    );
    drop(retained_offers);
    assert!(
        directory_owner.upgrade().is_some(),
        "the separately retained artifact custody still owns the acquired files"
    );
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    drop(retained_custody);
    assert!(
        directory_owner.upgrade().is_none(),
        "dropping the last custody-only support releases materialization"
    );
    assert!(!directory_path.exists());
}

#[test]
fn interface_only_full_availability_requires_explicit_original_acquisition() {
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
    let reusable_support = fixture_support_with_issued_offers(
        CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
        full.clone(),
        [offer.clone()],
    )
    .unwrap();
    let interfaces = reusable_support
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![entry.descriptor.owner.clone()])
        .unwrap();
    let interfaces = fixture_support_with_issued_offers(
        interfaces.projection().clone(),
        interfaces.artifacts().clone(),
        [offer.clone(), offer.clone()],
    )
    .unwrap();
    let row = interfaces.native_availability().rows().next().unwrap();
    assert!(row.permits(7) && row.permits(9001));
    assert!(matches!(row.mode, NativeTransportMode::Full));
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
            source_scope: Default::default(),
            template_imports: None,
        },
    ));
    let before = context.as_ref().clone();
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation(root.path(), PRODUCER)
        .unwrap();
    let inputs = request.compiler_inputs();
    let issued = inputs.native_availability(entry.descriptor.id).unwrap();
    assert!(matches!(&issued.mode, NativeTransportMode::Full));
    assert_eq!(
        issued.classification,
        NativeInputClassification::PrivateInput
    );
    assert_eq!(wire_mode(&request, "PrivateFullInterface"), full_mode());
    assert_eq!(
        wire_classification(&request, "PrivateFullInterface"),
        text("private-native")
    );
    assert!(wire_groups(&request)["PrivateFullInterface"].is_empty());
    let private_artifact = request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "PrivateFullInterface")
        .unwrap();
    assert_eq!(
        private_artifact
            .product
            .as_ref()
            .expect("private-native original keeps its product anchor")
            .module,
        "PrivateFullInterface"
    );
    request
        .validate_artifacts()
        .expect("the transported row owns the private-native product evidence");
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
    let transport_metadata = NativeCompilerTransport::lower_metadata(inputs).unwrap();
    assert!(transport_metadata
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
        compiler_execution_scope(&held_graph, &transport_metadata).unwrap(),
        held_graph,
        "private body metadata would incorrectly preserve this replay root if used as compiler policy"
    );
    let mut validation = PackageInterfaceValidation::default();
    let mut selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs,
        &validation.inventory,
    )
    .unwrap();
    let physical = crate::declaration_context::CapturedCompilerStage::issue(
        &mut selection,
        inputs.artifacts(),
        Vec::new(),
        &mut validation,
    )
    .unwrap();
    // Exercise the worker's raw receipt boundary, rather than an already
    // authenticated PendingImportOwner supplied directly to target admission.
    {
        use crate::certified_products::{AcceptedGlobal, ReceiptImportOwner};
        use tidepool_repr::execution_schema::{testing, GlobalDecl, RuntimeRep};
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
            let prepared = Arc::new(testing::prepare(wire).unwrap());
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
            let observed =
                AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory)
                    .unwrap();
            let result = inputs.artifacts().inventory().select_native_target(
                physical.physical_cohort(),
                observed,
                None,
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
            );
            if case == "exact" {
                assert!(
                    matches!(&result, Err(CompileError::ArtifactInventory(error))
                        if matches!(&error.failure, ArtifactInventoryFailure::NativeGroupUnavailable {
                            artifact, original_ordinal: 9001,
                        } if *artifact == entry.descriptor.id)
                        && matches!(&error.native_refusal,
                            Some(crate::artifact_inventory::NativeAdmissionRefusal::RootChoiceMissing { owner })
                                if owner == &entry.descriptor.owner)),
                    "InterfaceOnly custody cannot issue a Source root choice: {result:?}"
                );
            } else {
                if case == "wrong-version" {
                    assert!(
                        matches!(&result, Err(CompileError::CompilerEvidence(error))
                        if matches!(error.as_ref(), crate::certified_products::CertificationError::SourceOutsideCompilerSelection {
                            owner, requested_version, original_ordinal: None,
                        } if owner == &entry.descriptor.owner
                            && requested_version == &tidepool_repr::execution_schema::ModuleVersion([255; 32]))),
                        "wrong version must retain the requested identity: {result:?}"
                    );
                } else {
                    let expected = match case {
                        "unversioned" => "unversioned source lacks compiler selection",
                        "wrong-binder" => "target Source exact native membership",
                        _ => unreachable!(),
                    };
                    assert!(
                        matches!(result, Err(CompileError::CompilerEvidence(error))
                            if matches!(error.as_ref(), crate::certified_products::CertificationError::Mismatch(actual) if *actual == expected)),
                        "{case} must refuse at its owning boundary"
                    );
                }
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
        .complete_native_entries(&inputs.artifacts(), &validation.inventory)
        .unwrap()
        .is_empty());
    assert!(!cloned.permits_native_group(ArtifactId([255; 32]), native.owner(), 9001));
    assert!(!cloned.permits_native_group(
        entry.descriptor.id,
        product("WrongPrivateOwner", 128).owner(),
        9001
    ));
    let private = OriginalCompilerInputs::from_issued_support(inputs.compiler_source_support());
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
    let next_inputs = next.compiler_inputs();
    let next_availability = next_inputs
        .native_availability(entry.descriptor.id)
        .unwrap();
    assert!(matches!(&next_availability.mode, NativeTransportMode::Full));
    assert_eq!(
        next_availability.classification,
        NativeInputClassification::PrivateInput
    );
    assert!(next_inputs.artifacts().selected_native_groups().is_empty());
    let next_selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        next.compiler_inputs(),
        &validation.inventory,
    )
    .unwrap();
    assert!(next_selection.permits_native_group(entry.descriptor.id, native.owner(), 9001));
    let group = crate::certified_products::original_available_groups(&native)
        .unwrap()
        .find(|group| group.original_ordinal() == 9001)
        .unwrap();
    let binder = group.binders()[0].clone();
    // The worker observes the exact held pair independently of compiler policy.
    // Full private availability cannot supply an Original compiler role.
    let namespace = full
        .capture_graph_selection()
        .namespace
        .into_iter()
        .map(|binding| (binding.artifact, binding))
        .collect::<BTreeMap<_, _>>();
    let canonical = ArtifactEntry::canonical_artifact_id(native.module_interface().unwrap());
    let pair = |binding: &crate::artifact_inventory::ArtifactBinding| {
        Value::Array(vec![
            Value::Bytes(binding.artifact.0.to_vec()),
            Value::Bytes(binding.selection.0.to_vec()),
        ])
    };
    let mut demand_bytes = Vec::new();
    ciborium::ser::into_writer(
        &Value::Array(vec![
            text("TPNATIVEDEMAND1"),
            Value::Array(vec![Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                pair(&namespace[&canonical]),
                pair(&namespace[&entry.descriptor.id]),
                Value::Array(vec![
                    text(&binder.unit),
                    text(&binder.module),
                    text(&binder.namespace),
                    text(&binder.occurrence),
                    binder.record_parent.as_ref().map_or(Value::Null, text),
                ]),
            ])]),
        ]),
        &mut demand_bytes,
    )
    .unwrap();
    let packet =
        crate::certified_products::NativeDemandPacket::decode(&demand_bytes, &validation.inventory)
            .unwrap();
    let refused = next
        .inputs
        .compiler
        .resolve_native_demand(&packet, &mut validation)
        .err()
        .expect("TYPE-only compiler role cannot acquire an Original reference");
    assert!(
        matches!(&refused, CompileError::CompilerEvidence(error)
            if matches!(error.as_ref(), crate::certified_products::CertificationError::InputDecision {
                owner, reason: crate::certified_products::InputDecisionRefusal::RoleMismatch,
            } if owner == &entry.descriptor.owner)),
        "exact Original packet must refuse at its nominal role boundary: {refused:?}"
    );
    assert!(next_inputs
        .projection()
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert!(next_inputs.artifacts().selected_native_groups().is_empty());
    assert_eq!(context.as_ref(), &before);

    // Acquire the separately issued reusable compiler role explicitly. The
    // private availability above did not supply this role or choose a body.
    let reusable_context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: reusable_support,
            lexical: Default::default(),
            source_scope: Default::default(),
            template_imports: None,
        },
    ));
    let acquisition = ExactCompileContext::new(reusable_context)
        .prepare_compilation(&root.path().join("explicit-original"), PRODUCER)
        .unwrap();
    let acquisition_inputs = acquisition.compiler_inputs();
    let acquired = acquisition
        .inputs
        .compiler
        .resolve_native_demand(&packet, &mut validation)
        .expect("same exact packet uses the explicitly acquired reusable Original role");
    let bindings = crate::certified_products::OriginalBindingSelection::from_resolved(Arc::clone(
        acquired.groups().resolved_original_bindings().unwrap(),
    ));
    let product_inputs = ProductCompilerInputs::from_exact(acquisition_inputs)
        .with_original_bindings(bindings, &validation.inventory)
        .unwrap();
    let mut acquired_selection =
        crate::certified_products::CertifiedSourceSelection::from_product_compiler_inputs(
            &product_inputs,
            &validation.inventory,
        )
        .unwrap();
    let acquired_physical = crate::declaration_context::CapturedCompilerStage::issue(
        &mut acquired_selection,
        acquisition_inputs.artifacts(),
        Vec::new(),
        &mut validation,
    )
    .unwrap();
    let prepared = prepared_target_fixture(vec![GlobalDecl {
        identity: binder.clone(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: None,
        required_evaluated: false,
        required_generation: None,
    }]);
    let accepted = [AcceptedGlobal {
        identity: binder.clone(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: None,
        required_evaluated: false,
        owner: packet.originals()[0].clone(),
    }];
    let observed =
        AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory).unwrap();
    let mut expected_keys = acquired_physical
        .physical_cohort()
        .artifacts()
        .selected_native_groups();
    expected_keys.insert(NativeGroupKey {
        artifact: entry.descriptor.id,
        original_ordinal: 9001,
    });
    let demanded = acquisition_inputs
        .artifacts()
        .inventory()
        .select_native_target(
            acquired_physical.physical_cohort(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            &mut validation,
        )
        .expect("later target uses the same issuer's requested body");
    assert_eq!(
        demanded.artifacts().selected_native_groups(),
        expected_keys,
        "target preserves its actual inherited choices and selects ordinal 9001"
    );
    // The retained artifact view keeps inherited roots; resolution contains
    // only this target's exact demanded group and its dependency closure.
    let groups = demanded.resolve().unwrap().groups().clone();
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
fn private_native_scope_artifacts_keep_their_product_anchor() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("PrivateScopeProduct", 131);
    let plain_product = product("PlainScopeInterface", 132);
    let native_entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let plain_entry = Arc::new(ArtifactEntry::canonical(
        plain_product.module_interface().unwrap().clone(),
    ));
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(
            &inventory.empty_view(),
            vec![native_entry.clone(), plain_entry.clone()],
        )
        .unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(native_entry.descriptor.id, &full)
        .unwrap()
        .unwrap();
    let offer = IssuedNativeOffer::issue(native_entry.descriptor.id, &full, Some(proof)).unwrap();
    let issued = recover_fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&[native_entry.clone(), plain_entry.clone()])
            .unwrap(),
        full.clone(),
        [],
    )
    .unwrap();
    // Both owners retain interface-only compiler roles; the native original is
    // available only through the issued offer, as in the failing exact scope.
    let interfaces = issued
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![
            native_entry.descriptor.owner.clone(),
            plain_entry.descriptor.owner.clone(),
        ])
        .unwrap();
    let support = fixture_support_with_issued_offers(
        interfaces.projection().clone(),
        interfaces.artifacts().clone(),
        [offer],
    )
    .unwrap();
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: support,
            lexical: Default::default(),
            source_scope: Default::default(),
            template_imports: None,
        },
    ));
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context)
        .prepare_compilation(root.path(), PRODUCER)
        .unwrap();
    // The scope advertises the module as a full private-native carrier...
    assert_eq!(wire_mode(&request, "PrivateScopeProduct"), full_mode());
    assert_eq!(
        wire_classification(&request, "PrivateScopeProduct"),
        text("private-native")
    );
    // ...so its request artifact keeps the product anchor the availability row
    // carries instead of the compiler's canonical role demoting it.
    let artifact = request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "PrivateScopeProduct")
        .unwrap();
    let product = artifact
        .product
        .as_ref()
        .expect("private-native original keeps its product anchor");
    assert_eq!(product.module, "PrivateScopeProduct");
    assert!(
        product.path.to_string_lossy().ends_with(".products.cbor"),
        "product anchor is the products sidecar: {:?}",
        product.path
    );
    assert_eq!(
        crate::checked_cell::hash(&std::fs::read(&product.path).unwrap()),
        product.sha256
    );
    // A compiler-role interface stays product-free.
    let plain = request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "PlainScopeInterface")
        .unwrap();
    assert!(
        plain.product.is_none(),
        "a canonical interface carries no executable product"
    );
    request.validate_artifacts().unwrap();
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
            NativeInputSelection::issue_checked(&projection, &metadata, &full, &offers).unwrap();
        assert!(
            !transport.availability.0.contains_key(&entry.descriptor.id),
            "neither complete byte custody nor unions of subsets create a Full witness"
        );
        let ArtifactPayload::Original(original) = &entry.payload else {
            unreachable!()
        };
        assert!(transport
            .candidates
            .select_all()
            .unwrap()
            .receipt_original(
                &transport.availability,
                &original.owner().unit,
                &original.owner().module,
                &original.owner().module_version
            )
            .unwrap()
            .is_none());
    }
}

#[test]
fn direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let products = [
        product("AdvertisedHistory", 71),
        product("AdvertisedHistory", 72),
    ];
    assert_eq!(
        products[0].module_interface(),
        products[1].module_interface()
    );
    let entries = products
        .iter()
        .map(|product| Arc::new(ArtifactEntry::original(producer, product.clone()).unwrap()))
        .collect::<Vec<_>>();
    assert_ne!(entries[0].descriptor.id, entries[1].descriptor.id);
    let complete = entries
        .iter()
        .map(|entry| {
            let inventory = ArtifactInventory::default();
            let view = inventory
                .admit_shared(&inventory.empty_view(), vec![entry.clone()])
                .unwrap();
            fixture_compiler_support(
                CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
                view,
            )
        })
        .collect::<Vec<_>>();
    let owner = entries[0].descriptor.owner.clone();
    let canonical = complete[0].projection().roles()[0].interface();
    let binding = |support: &crate::artifact_inventory::CompilerSourceSupport| {
        support
            .artifacts()
            .capture_graph_selection()
            .namespace
            .into_iter()
            .find(|binding| binding.artifact == canonical)
            .unwrap()
    };
    assert_ne!(
        binding(&complete[0]),
        binding(&complete[1]),
        "equal canonical bytes have distinct admitted bindings"
    );
    let interfaces = complete
        .iter()
        .map(|support| {
            support
                .selected_interfaces()
                .unwrap()
                .support_for_owners(vec![owner.clone()])
                .unwrap()
                .retain_template_availability(support)
                .unwrap()
        })
        .collect::<Vec<_>>();
    for winner in 0..2 {
        let joined = interfaces[winner].merge(&interfaces[1 - winner]).unwrap();
        assert_eq!(binding(&joined), binding(&complete[winner]));
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&joined)
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context))
            .prepare_compilation(root.path(), PRODUCER)
            .unwrap();
        let inputs = request.compiler_inputs();
        assert_eq!(
            request.transport_metadata.entries[&owner].descriptor.id,
            entries[winner].descriptor.id
        );
        assert!(
            inputs
                .projection()
                .roles()
                .iter()
                .all(|role| role.original().is_none()),
            "advertisement does not restore source authority"
        );
        for entry in &entries {
            let row = inputs
                .native_availability(entry.descriptor.id)
                .expect("historical normalized row retained");
            assert!(row.permits(7) && row.permits(9001));
        }
        let availability = inputs.private_native_availability();
        let direct = inputs.advertised_native_originals().unwrap();
        for index in 0..2 {
            let native = products[index].owner();
            let row = direct
                .receipt_original(
                    &availability,
                    &native.unit,
                    &native.module,
                    &native.module_version,
                )
                .unwrap();
            assert_eq!(row.is_some(), index == winner);
        }
        let validation = PackageInterfaceValidation::default();
        let receipt = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            inputs,
            &validation.inventory,
        )
        .unwrap();
        assert_eq!(
            receipt
                .target_original(products[winner].owner())
                .unwrap()
                .unwrap()
                .descriptor
                .id,
            entries[winner].descriptor.id
        );
        let canonical_only = ArtifactInventory::default();
        let canonical_entry = Arc::new(ArtifactEntry::canonical(
            products[0].module_interface().unwrap().clone(),
        ));
        let unrelated = canonical_only
            .admit_shared(&canonical_only.empty_view(), vec![canonical_entry])
            .unwrap()
            .retain_custody(joined.artifacts())
            .unwrap();
        let projection = joined.projection();
        let metadata = projection
            .project_metadata(unrelated.metadata_snapshot())
            .unwrap();
        let transport = NativeInputSelection::receive(
            projection,
            &metadata,
            &unrelated,
            joined.native_availability(),
            joined.native_offers(),
        )
        .unwrap();
        assert!(matches!(
            metadata.entries[&owner].payload,
            ArtifactPayload::Canonical(_)
        ));
        assert!(
            transport.candidates.select_all().unwrap().0.is_empty(),
            "same canonical ID in another scope has no affine direct candidate"
        );
        assert_eq!(transport.availability.0.len(), 2);
        let selected = complete[winner]
            .artifacts()
            .retain_custody(complete[1 - winner].artifacts())
            .unwrap();
        let selected_projection = complete[winner].projection();
        let metadata = selected_projection
            .project_metadata(selected.metadata_snapshot())
            .unwrap();
        let transport = NativeInputSelection::receive(
            selected_projection,
            &metadata,
            &selected,
            joined.native_availability(),
            joined.native_offers(),
        )
        .expect("an explicit Original displaces an unconsumed historical availability root");
        assert_eq!(
            transport
                .availability
                .0
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([entries[winner].descriptor.id])
        );
        assert_eq!(
            transport.candidates.select_all().unwrap().0.get(&owner),
            Some(&entries[winner].descriptor.id)
        );
        assert!(
            selected
                .artifact_ids()
                .contains(&entries[1 - winner].descriptor.id),
            "pruning optional availability does not discard historical byte custody"
        );
        assert_eq!(joined.native_availability().rows().count(), 2);
    }
}

#[test]
fn direct_native_advertisement_uses_descendant_contribution_without_sibling_custody() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let hidden = product("AdvertisedDescendant", 81);
    let root = dependent_product(
        "AdvertisedRoot",
        82,
        vec![(3, vec![source_import(&hidden, 7)])],
    );
    let sibling = product("AdvertisedSibling", 83);
    let products = recovered_witness_fixtures(&[hidden, root, sibling])
        .into_iter()
        .map(|witness| witness.product)
        .collect::<Vec<_>>();
    let inputs = (|| -> Result<OriginalCompilerInputs, CompileError> {
        let fixture_products: &[CertifiedRecoveryProduct] = &products;
        let entries = fixture_products
            .iter()
            .map(|product| ArtifactEntry::original(producer, product.clone()).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        let inventory = ArtifactInventory::default();
        let artifacts = inventory.admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )?;
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
        CapturedCompilerStage::issue(&mut selection, &artifacts, Vec::new(), &mut validation)
            .map(|stage| stage.compiler_inputs().as_ref().clone())
    })()
    .unwrap();
    let complete =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&inputs).unwrap();
    let ids = products
        .iter()
        .map(|product| {
            ArtifactEntry::original(producer, product.clone())
                .unwrap()
                .descriptor
                .id
        })
        .collect::<Vec<_>>();
    let owners = products
        .iter()
        .map(|product| identity(&product.owner().unit, &product.owner().module))
        .collect::<Vec<_>>();
    let projection = complete
        .projection()
        .for_source_owners(&BTreeSet::from([owners[1].clone()]));
    let root_offer = complete
        .native_offers()
        .iter()
        .find(|offer| offer.original() == ids[1])
        .unwrap()
        .clone();
    assert!(root_offer.is_full());
    assert!(complete.artifacts().selected_native_groups().is_empty());
    let rows = RequestNativeAvailability::issue_checked(
        &projection,
        complete.artifacts(),
        std::slice::from_ref(&root_offer),
    )
    .unwrap();
    let issued =
        recover_fixture_compiler_support(projection, complete.artifacts().clone(), [root_offer])
            .unwrap()
            .retain_normalized_availability(&rows)
            .unwrap();
    assert!(
        issued.artifacts().selected_native_groups().is_empty(),
        "availability does not add executable demand"
    );
    assert!(
        issued
            .native_offers()
            .iter()
            .all(|offer| offer.original() == ids[1]),
        "only the real R issuer contributes native proof"
    );
    let hidden_row = issued
        .native_availability()
        .rows()
        .find(|row| row.original.descriptor.id == ids[0])
        .unwrap();
    assert!(hidden_row.permits(7) && !hidden_row.permits(9001));
    assert!(hidden_row.offers().all(|offer| offer.original() == ids[1]));
    let types = issued
        .selected_interfaces()
        .unwrap()
        .support_for_owners(owners.clone())
        .unwrap()
        .retain_template_availability(&issued)
        .unwrap();
    let context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_issued_compiler_support(&types)
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(Arc::new(context))
        .prepare_compilation(root.path(), PRODUCER)
        .unwrap();
    let inputs = request.compiler_inputs();
    assert_eq!(
        request.transport_metadata.entries[&owners[0]].descriptor.id,
        ids[0]
    );
    assert!(
        matches!(
            request.transport_metadata.entries[&owners[2]].payload,
            ArtifactPayload::Canonical(_)
        ),
        "R's broad checked custody cannot advertise its unrelated sibling"
    );
    assert!(inputs.native_availability(ids[2]).is_none());
    assert_eq!(wire_mode(&request, &owners[0].module), issued_mode(&[7]));
    let private = inputs.private_native_availability();
    let direct = inputs.advertised_native_originals().unwrap();
    let receipt = direct
        .receipt_original(
            &private,
            &owners[0].unit,
            &owners[0].module,
            &products[0].owner().module_version,
        )
        .unwrap()
        .unwrap();
    assert!(receipt.permits(7) && !receipt.permits(9001));
}

fn independently_cloned_availability(
    rows: &RequestNativeAvailability,
) -> RequestNativeAvailability {
    RequestNativeAvailability::from_issued_rows(rows.rows().map(|row| {
        let mut copied = (**row).clone();
        copied.contributions = row
            .contributions
            .iter()
            .map(|contribution| Arc::new((**contribution).clone()))
            .collect::<Vec<_>>()
            .into();
        Arc::new(copied)
    }))
    .unwrap()
}

// Cold independent oracle: rebuild durable facts from the actual row owners,
// including freshly hashing each contribution rather than borrowing the map or
// the production row-fact constructor.
fn recompute_native_availability_facts(
    rows: &RequestNativeAvailability,
) -> imbl::OrdMap<ArtifactId, NativeAvailabilitySnapshotFacts> {
    rows.rows()
        .map(|row| {
            let ArtifactPayload::Original(product) = &row.original.payload else {
                panic!("native row original payload")
            };
            let contributions = row
                .contributions
                .iter()
                .map(|contribution| {
                    let facts = contribution.snapshot_facts().unwrap();
                    (facts.identity().unwrap(), facts)
                })
                .collect();
            (
                row.original.descriptor.id,
                NativeAvailabilitySnapshotFacts {
                    original: row.original.descriptor.id,
                    owner: product.owner().clone(),
                    mode: row.mode.clone(),
                    contributions,
                },
            )
        })
        .collect()
}

#[test]
fn native_availability_snapshot_facts_follow_changed_rows_without_hot_census() {
    let native = product("PersistentNativeFacts", 53);
    let context = native_context(&native);
    let support = context.compiler_source_support();
    let rows = support.native_availability();
    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    let facts = rows.snapshot_facts().unwrap();
    for _ in 0..8 {
        let snapshot = rows.snapshot_facts().unwrap();
        assert!(facts.ptr_eq(&snapshot));
        assert_eq!(snapshot, recompute_native_availability_facts(rows));
    }
    assert_eq!(
        NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get),
        before
    );

    let copied = independently_cloned_availability(rows);
    assert_eq!(copied.snapshot_facts().unwrap(), facts);
    assert!(!Arc::ptr_eq(&rows.0, &copied.0));
    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    let unchanged = rows.merge(&copied).unwrap().into_rows();
    assert!(Arc::ptr_eq(&rows.0, &unchanged.0));
    assert!(facts.ptr_eq(&unchanged.snapshot_facts().unwrap()));
    assert_eq!(
        NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get),
        before
    );

    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    let classified = rows
        .for_compiler_roles(&support.projection().interface_only(), support.artifacts())
        .unwrap();
    assert!(classified
        .rows()
        .all(|row| row.classification == NativeInputClassification::PrivateInput));
    assert!(facts.ptr_eq(&classified.snapshot_facts().unwrap()));
    assert_eq!(
        classified.snapshot_facts().unwrap(),
        recompute_native_availability_facts(&classified)
    );
    assert_eq!(
        NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get),
        before
    );

    // An equal canonical payload in a different namespace binding withdraws
    // Full qualification. This existing owner operation changes only one row.
    let canonical = Arc::new(ArtifactEntry::canonical(
        native.module_interface().unwrap().clone(),
    ));
    let projection = CompilerInputProjection::from_issued_entries(&[canonical.clone()]).unwrap();
    let inventory = ArtifactInventory::default();
    let rebound = inventory
        .admit_shared(&inventory.empty_view(), vec![canonical])
        .unwrap()
        .retain_custody(support.artifacts())
        .unwrap();
    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    let downgraded = rows.for_compiler_roles(&projection, &rebound).unwrap();
    let after = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    assert_eq!(after, (before.0, before.1 + 1));
    assert_eq!(
        downgraded.rows().next().unwrap().mode,
        NativeTransportMode::IssuedGroups
    );
    assert_eq!(
        downgraded.snapshot_facts().unwrap(),
        recompute_native_availability_facts(&downgraded)
    );
    assert_ne!(downgraded.snapshot_facts().unwrap(), facts);
    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    let promoted = downgraded.merge(rows).unwrap().into_rows();
    let after = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    assert_eq!(after, (before.0, before.1 + 1));
    assert_eq!(promoted.snapshot_facts().unwrap(), facts);
    assert_eq!(
        promoted.snapshot_facts().unwrap(),
        recompute_native_availability_facts(&promoted)
    );

    // A one-row change beside an unchanged row updates one persistent path,
    // rather than rebuilding facts for every row in the receiving domain.
    let extra = native_context(&product("PersistentOtherNativeFacts", 52));
    let extra_support = extra.compiler_source_support();
    let extra_id = extra_support
        .native_availability()
        .rows()
        .next()
        .unwrap()
        .original
        .descriptor
        .id;
    let joined = rows
        .merge(extra_support.native_availability())
        .unwrap()
        .into_rows();
    let canonical = Arc::new(ArtifactEntry::canonical(
        native.module_interface().unwrap().clone(),
    ));
    let mixed_roles = CompilerInputProjection::from_issued_entries(&[canonical.clone()])
        .unwrap()
        .merge(extra_support.projection())
        .unwrap();
    let mixed_view = extra_support
        .artifacts()
        .inventory()
        .admit_shared_with_demand(
            extra_support.artifacts(),
            vec![canonical],
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap()
        .retain_custody(support.artifacts())
        .unwrap();
    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    let mixed = joined
        .for_compiler_roles(&mixed_roles, &mixed_view)
        .unwrap();
    assert_eq!(
        NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get),
        (before.0, before.1 + 1)
    );
    assert_eq!(mixed.rows().count(), 2);
    assert!(Arc::ptr_eq(&joined.0[&extra_id], &mixed.0[&extra_id]));
    assert!(joined.1[&extra_id]
        .contributions
        .ptr_eq(&mixed.1[&extra_id].contributions));
    assert_eq!(
        mixed.snapshot_facts().unwrap(),
        recompute_native_availability_facts(&mixed)
    );

    // Acquisition authenticates the same facts against each actual input owner;
    // sharing immutable facts cannot share the acquired rows or opaque offers.
    let acquired_again = native_context(&native);
    let mut restored = Vec::new();
    let before = NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get);
    for acquired in [support, acquired_again.compiler_source_support()] {
        let offers = acquired
            .native_offers()
            .iter()
            .map(|offer| (offer.snapshot_facts().identity().unwrap(), offer.clone()))
            .collect();
        let rows =
            RequestNativeAvailability::restore(&facts, &offers, acquired.artifacts()).unwrap();
        assert_eq!(
            rows.snapshot_facts().unwrap(),
            recompute_native_availability_facts(&rows)
        );
        assert!(facts.ptr_eq(&rows.snapshot_facts().unwrap()));
        assert!(Arc::ptr_eq(
            &rows.rows().next().unwrap().original,
            &acquired.artifacts().metadata_snapshot().artifacts
                [&rows.rows().next().unwrap().original.descriptor.id]
        ));
        restored.push(rows);
    }
    assert_eq!(
        NATIVE_AVAILABILITY_FACT_WORK.with(std::cell::Cell::get),
        (before.0 + 2, before.1)
    );
    assert!(!Arc::ptr_eq(&restored[0].0, &restored[1].0));
    assert!(!Arc::ptr_eq(
        restored[0].rows().next().unwrap(),
        restored[1].rows().next().unwrap()
    ));
    assert!(!Arc::ptr_eq(
        &restored[0].rows().next().unwrap().original,
        &restored[1].rows().next().unwrap().original
    ));
    assert!(!restored[0].rows().next().unwrap().contributions[0]
        .offer()
        .same_issuance(restored[1].rows().next().unwrap().contributions[0].offer()));
    drop(context);
    drop(acquired_again);
    drop(restored.remove(0));
    let surviving = restored[0].rows().next().unwrap();
    surviving.validate_full_evidence().unwrap();
    assert!(surviving.permits(9001));
}

#[test]
fn unchanged_native_availability_merge_keeps_qualified_rows() {
    let native = product("UnchangedNativeRows", 54);
    let context = native_context(&native);
    let support = context.compiler_source_support();
    let rows = support.native_availability();
    assert_eq!(rows.rows().count(), 1);
    assert_eq!(rows.rows().next().unwrap().mode, NativeTransportMode::Full);
    let independent = independently_cloned_availability(rows);
    assert_eq!(
        rows.snapshot_facts().unwrap(),
        recompute_native_availability_facts(rows)
    );
    let expected = rows
        .for_compiler_roles(support.projection(), support.artifacts())
        .unwrap();
    assert_eq!(
        &expected, rows,
        "the receiver already carries completed qualification"
    );
    for (prior, copied) in rows.rows().zip(independent.rows()) {
        for (prior, copied) in prior.contributions.iter().zip(copied.contributions.iter()) {
            assert!(!Arc::ptr_eq(prior, copied));
        }
    }
    assert!(!Arc::ptr_eq(
        rows.rows().next().unwrap(),
        independent.rows().next().unwrap(),
    ));
    for incoming in [
        RequestNativeAvailability::default(),
        rows.clone(),
        independent,
    ] {
        let merged = rows.merge(&incoming).unwrap();
        let NativeAvailabilityMerge::Unchanged(unchanged) = merged else {
            panic!("an equal row merge must preserve the existing row owner");
        };
        assert!(Arc::ptr_eq(&rows.0, &unchanged.0));
        let before = support.artifacts().inventory().metrics();
        let retained = support.retain_normalized_availability(&incoming).unwrap();
        let after = support.artifacts().inventory().metrics();
        assert!(Arc::ptr_eq(&rows.0, &retained.native_availability().0));
        assert_eq!(retained.native_availability(), &expected);
        assert!(Arc::ptr_eq(
            support.artifacts().selection_snapshot(),
            retained.artifacts().selection_snapshot(),
        ));
        assert_eq!(after.graph_visits, before.graph_visits);
    }

    let extra = native_context(&product("ChangedNativeRows", 55));
    assert!(matches!(
        rows.merge(extra.compiler_source_support().native_availability())
            .unwrap(),
        NativeAvailabilityMerge::Changed(_)
    ));
    let before = support.artifacts().inventory().metrics();
    let requalified = support
        .retain_normalized_availability(extra.compiler_source_support().native_availability())
        .unwrap();
    let after = support.artifacts().inventory().metrics();
    assert!(!Arc::ptr_eq(&rows.0, &requalified.native_availability().0));
    assert!(after.graph_visits > before.graph_visits);
    assert_eq!(requalified.native_availability(), rows);

    let canonical = Arc::new(ArtifactEntry::canonical(
        native.module_interface().unwrap().clone(),
    ));
    let canonical_id = canonical.descriptor.id;
    let projection = CompilerInputProjection::from_issued_entries(&[canonical.clone()]).unwrap();
    let inventory = ArtifactInventory::default();
    let canonical_only = inventory
        .admit_shared(&inventory.empty_view(), vec![canonical])
        .unwrap();
    let rebound = canonical_only.retain_custody(support.artifacts()).unwrap();
    assert_ne!(
        support
            .artifacts()
            .selection_snapshot()
            .namespace
            .get(&canonical_id),
        rebound.selection_snapshot().namespace.get(&canonical_id),
        "equal canonical payloads must still have distinct issued bindings in this control",
    );
    let downgraded = rows.for_compiler_roles(&projection, &rebound).unwrap();
    let downgraded_row = downgraded.rows().next().unwrap();
    assert_eq!(downgraded_row.mode, NativeTransportMode::IssuedGroups);
    assert!(downgraded_row.permits(7));
    assert!(downgraded_row.permits(9001));
    assert!(!downgraded_row.permits(9999));
    assert!(matches!(
        downgraded.merge(rows).unwrap(),
        NativeAvailabilityMerge::Changed(_)
    ));
    let rebound_support = recover_fixture_compiler_support(
        projection,
        rebound,
        support.native_offers().iter().cloned(),
    )
    .unwrap()
    .retain_normalized_availability(&downgraded)
    .unwrap();
    let expected = rebound_support
        .native_availability()
        .for_compiler_roles(rebound_support.projection(), rebound_support.artifacts())
        .unwrap();
    assert_eq!(&expected, rebound_support.native_availability());
    let unchanged = rebound_support
        .retain_normalized_availability(&independently_cloned_availability(
            rebound_support.native_availability(),
        ))
        .unwrap();
    assert!(Arc::ptr_eq(
        &rebound_support.native_availability().0,
        &unchanged.native_availability().0
    ));
    assert_eq!(unchanged.native_availability(), &expected);
    let prior = rebound_support.native_availability().0.clone();
    let retained = rebound_support
        .retain_normalized_availability(rows)
        .unwrap();
    assert!(!Arc::ptr_eq(&prior, &retained.native_availability().0));
    let retained_row = retained.native_availability().rows().next().unwrap();
    assert_eq!(retained_row.mode, NativeTransportMode::IssuedGroups);
    assert!(
        !retained_row.permits(9999),
        "a changed merge must recheck Full's exact namespace"
    );
}

#[test]
fn unchanged_native_availability_merge_preserves_indirect_permissions() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let hidden = product("UnchangedIndirectHidden", 58);
    let root = dependent_product(
        "UnchangedIndirectRoot",
        59,
        vec![(3, vec![source_import(&hidden, 7)])],
    );
    let products = recovered_witness_fixtures(&[hidden, root])
        .into_iter()
        .map(|witness| witness.product)
        .collect::<Vec<_>>();
    let inputs = (|| -> Result<OriginalCompilerInputs, CompileError> {
        let fixture_products: &[CertifiedRecoveryProduct] = &products;
        let entries = fixture_products
            .iter()
            .map(|product| {
                ArtifactEntry::original(producer.sha256(), product.clone()).map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let inventory = ArtifactInventory::default();
        let artifacts = inventory.admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )?;
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
        CapturedCompilerStage::issue(&mut selection, &artifacts, Vec::new(), &mut validation)
            .map(|stage| stage.compiler_inputs().as_ref().clone())
    })()
    .unwrap();
    let complete =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&inputs).unwrap();
    let root_owner = identity(&products[1].owner().unit, &products[1].owner().module);
    let selected = complete
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![root_owner])
        .unwrap();
    let support = selected.retain_template_availability(&complete).unwrap();
    let hidden_owner = identity(&products[0].owner().unit, &products[0].owner().module);
    assert!(!support
        .projection()
        .snapshot_roles()
        .contains_key(&hidden_owner));
    let hidden_id = ArtifactEntry::original(producer.sha256(), products[0].clone())
        .unwrap()
        .descriptor
        .id;
    let rows = support.native_availability();
    let hidden_row = &rows.0[&hidden_id];
    assert_eq!(hidden_row.mode, NativeTransportMode::IssuedGroups);
    assert!(hidden_row.permits(7));
    assert!(!hidden_row.permits(9001));
    assert_eq!(
        rows.snapshot_facts().unwrap(),
        recompute_native_availability_facts(rows)
    );
    let expected = rows
        .for_compiler_roles(support.projection(), support.artifacts())
        .unwrap();
    assert_eq!(
        &expected, rows,
        "indirect permission restriction is already complete"
    );
    let independent = independently_cloned_availability(rows);
    let before = support.artifacts().inventory().metrics();
    let retained = support
        .retain_normalized_availability(&independent)
        .unwrap();
    let after = support.artifacts().inventory().metrics();
    assert!(Arc::ptr_eq(&rows.0, &retained.native_availability().0));
    assert_eq!(retained.native_availability(), &expected);
    assert_eq!(after.graph_visits, before.graph_visits);
    assert!(retained.native_availability().0[&hidden_id].permits(7));
    assert!(!retained.native_availability().0[&hidden_id].permits(9001));
}

#[test]
fn native_availability_failed_merge_preserves_qualified_owner() {
    let contexts = [
        native_context(&product("AtomicNativeRowsA", 56)),
        native_context(&product("AtomicNativeRowsB", 57)),
    ];
    let mut rows = contexts
        .iter()
        .enumerate()
        .map(|(index, context)| {
            let row = Arc::clone(
                context
                    .compiler_source_support()
                    .native_availability()
                    .rows()
                    .next()
                    .unwrap(),
            );
            (row.original.descriptor.id, (index, row))
        })
        .collect::<BTreeMap<_, _>>()
        .into_values();
    let (_, lower) = rows.next().unwrap();
    let (upper_index, upper) = rows.next().unwrap();
    assert!(lower.original.descriptor.id < upper.original.descriptor.id);
    assert_ne!(lower.canonical, upper.canonical);
    let mut invalid = (*upper).clone();
    invalid.canonical = lower.canonical;
    let incoming = RequestNativeAvailability::from_issued_rows([lower, Arc::new(invalid)]).unwrap();
    let support = contexts[upper_index].compiler_source_support();
    let before = support.native_availability().clone();
    let incoming_before = incoming.clone();
    let failure = support
        .retain_normalized_availability(&incoming)
        .unwrap_err();
    assert!(
        matches!(failure, CompileError::ExtractFailed(_)),
        "actual_error={failure:?}"
    );
    assert!(Arc::ptr_eq(&before.0, &support.native_availability().0));
    assert_eq!(support.native_availability(), &before);
    assert_eq!(incoming, incoming_before);
}

#[test]
fn private_full_candidates_survive_receive_and_refuse_ambiguous_selection() {
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
    let separate_rows = views
        .iter()
        .zip(&offers)
        .map(|(view, offer)| {
            RequestNativeAvailability::issue_checked(&projection, view, std::slice::from_ref(offer))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let normalized = separate_rows[0]
        .merge(&separate_rows[1])
        .unwrap()
        .into_rows();
    for reversed in [false, true] {
        let selected_offers = if reversed {
            vec![offers[1].clone(), offers[0].clone()]
        } else {
            offers.clone()
        };
        let normalized = if reversed {
            separate_rows[1]
                .merge(&separate_rows[0])
                .unwrap()
                .into_rows()
        } else {
            normalized.clone()
        };
        let received = NativeInputSelection::receive(
            &projection,
            &metadata,
            &custody,
            &normalized,
            &selected_offers,
        )
        .expect("cold receive preserves each independently issued historical body");
        let owner = &entries[0].descriptor.owner;
        assert_eq!(received.availability.0.len(), 2);
        assert_eq!(
            received.candidates.0[owner]
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            entries.iter().map(|entry| entry.descriptor.id).collect()
        );
        let error = received.candidates.require_unique(owner).unwrap_err();
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { owner: ref ambiguous }
                if ambiguous == owner)));
        let support = recover_fixture_compiler_support(
            projection.clone(),
            custody.clone(),
            selected_offers.iter().cloned(),
        )
        .unwrap()
        .retain_normalized_availability(&normalized)
        .unwrap();
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&support)
            .unwrap()
            .admit_producer(producer.sha256())
            .unwrap();
        let cold = context
            .capture_world_primary_compiler_inputs(producer.sha256())
            .unwrap();
        let before = NATIVE_TRANSPORT_LOWERINGS.with(std::cell::Cell::get);
        let error = NativeCompilerTransport::lower_metadata(&cold)
            .err()
            .expect("finite compiler selection must refuse competing bodies");
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { owner: ref ambiguous }
                if ambiguous == owner)));
        assert_eq!(
            NATIVE_TRANSPORT_LOWERINGS.with(std::cell::Cell::get),
            before
        );

        let first = usize::from(reversed);
        let initial_support = recover_fixture_compiler_support(
            projection.clone(),
            custody.clone(),
            [offers[first].clone()],
        )
        .unwrap()
        .retain_normalized_availability(&separate_rows[first])
        .unwrap();
        let initial_context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&initial_support)
            .unwrap()
            .admit_producer(producer.sha256())
            .unwrap();
        let initial = initial_context
            .capture_world_primary_compiler_inputs(producer.sha256())
            .unwrap();
        assert_eq!(
            initial
                .native_selection
                .candidates
                .require_unique(owner)
                .unwrap(),
            Some(entries[first].descriptor.id)
        );
        let advanced = initial
            .advance_program_support(&Arc::new(context), &support)
            .unwrap();
        assert!(advanced.changed_owners.is_empty(), "adding a historical candidate changes availability without changing the selected canonical role");
        assert_eq!(
            advanced.inputs.native_selection.candidates.0,
            cold.native_selection.candidates.0
        );
        assert_eq!(
            advanced.inputs.native_selection.availability,
            cold.native_selection.availability
        );
        assert!(
            matches!(advanced.inputs.native_selection.candidates.require_unique(owner),
            Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { owner: ref ambiguous }
                if ambiguous == owner))
        );

        // Fresh Full qualification still requires one exact body per namespace;
        // retained competing choices cannot grant a new combined Full offer.
        let error =
            NativeInputSelection::issue_checked(&projection, &metadata, &custody, &selected_offers)
                .err()
                .expect("type-only role cannot choose among distinct qualified historical Fulls");
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { ref owner }
                if owner == &entries[0].descriptor.owner)));
    }
    let duplicate = NativeInputSelection::issue_checked(
        &projection,
        &metadata,
        &custody,
        &[offers[0].clone(), offers[0].clone()],
    )
    .unwrap();
    assert!(duplicate.availability.0[&entries[0].descriptor.id].permits(9001));
    let original = duplicate.availability.0[&entries[0].descriptor.id].original_owner();
    let private = duplicate.availability.clone();
    let receipt_owner = duplicate
        .candidates
        .select_all()
        .unwrap()
        .receipt_original(
            &private,
            &original.unit,
            &original.module,
            &original.module_version,
        )
        .unwrap()
        .unwrap();
    assert_eq!(receipt_owner.original_owner(), original);
    assert!(receipt_owner.permits(9001));
    for reversed in [false, true] {
        let selected_offers = if reversed {
            vec![offers[1].clone(), offers[0].clone()]
        } else {
            offers.clone()
        };
        for index in 0..2 {
            let selected =
                CompilerInputProjection::from_issued_entries(&[entries[index].clone()]).unwrap();
            let view = views[index].retain_custody(&views[1 - index]).unwrap();
            let metadata = selected.project_metadata(view.metadata_snapshot()).unwrap();
            for transport in [
                NativeInputSelection::receive(
                    &selected,
                    &metadata,
                    &view,
                    &normalized,
                    &selected_offers,
                )
                .unwrap(),
                NativeInputSelection::issue_checked(&selected, &metadata, &view, &selected_offers)
                    .unwrap(),
            ] {
                assert!(transport.availability.0[&entries[index].descriptor.id].permits(9001));
                assert!(!transport
                    .availability
                    .0
                    .contains_key(&entries[1 - index].descriptor.id));
                assert_eq!(
                    transport
                        .candidates
                        .require_unique(&entries[index].descriptor.owner)
                        .unwrap(),
                    Some(entries[index].descriptor.id)
                );
                assert_eq!(
                    transport.candidates.select_all().unwrap().0[&entries[index].descriptor.owner],
                    entries[index].descriptor.id
                );
            }
        }
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
fn program_offer_only_append_refreshes_empty_native_candidates() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeArtifactDemand};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("OfferOnlyCandidate", 206);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let canonical = Arc::new(ArtifactEntry::canonical(
        native.module_interface().unwrap().clone(),
    ));
    let inventory = ArtifactInventory::default();
    let views = ["OfferOnlyLeft", "OfferOnlyRight"].map(|marker| {
        inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![
                    entry.clone(),
                    Arc::new(ArtifactEntry::canonical(
                        product(marker, 207).module_interface().unwrap().clone(),
                    )),
                ],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap()
    });
    let offers = views.each_ref().map(|view| {
        let offer = IssuedNativeOffer::issue(entry.descriptor.id, view, None).unwrap();
        assert!(!offer.is_full());
        assert!(offer.proof().is_none());
        offer
    });
    assert_ne!(
        offers[0], offers[1],
        "the two Empty offers have distinct exact carrier scopes"
    );
    let original_role = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let type_role = CompilerInputProjection::from_issued_entries(&[canonical.clone()]).unwrap();
    for old in 0..2 {
        let current = 1 - old;
        let empty = RequestNativeAvailability::issue_checked(
            &original_role,
            &views[old],
            std::slice::from_ref(&offers[old]),
        )
        .unwrap();
        assert!(empty
            .issued_row(entry.descriptor.id)
            .unwrap()
            .issued_ordinals()
            .is_empty());
        let custody = views[current].retain_custody(&views[old]).unwrap();
        let prior_support =
            recover_fixture_compiler_support(type_role.clone(), custody, [offers[old].clone()])
                .unwrap()
                .retain_normalized_availability(&empty)
                .unwrap();
        assert!(!offers[old].matches_direct_canonical_binding(
            entry.descriptor.id,
            canonical.descriptor.id,
            prior_support.artifacts(),
        ));
        assert!(offers[current].matches_direct_canonical_binding(
            entry.descriptor.id,
            canonical.descriptor.id,
            prior_support.artifacts(),
        ));
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_issued_compiler_support(&prior_support)
                .unwrap()
                .admit_producer(producer.sha256())
                .unwrap(),
        );
        let captured = context
            .capture_world_primary_compiler_inputs(producer.sha256())
            .unwrap();
        let owner = &entry.descriptor.owner;
        let prior_row = captured.native_availability(entry.descriptor.id).unwrap();
        assert_eq!(prior_row.mode, NativeTransportMode::IssuedGroups);
        assert!(prior_row.issued_ordinals().is_empty());
        assert_eq!(
            captured
                .native_selection
                .candidates
                .require_unique(owner)
                .unwrap(),
            None
        );
        let incoming = recover_fixture_compiler_support(
            type_role.clone(),
            views[current].clone(),
            [offers[current].clone()],
        )
        .unwrap();
        assert!(incoming.native_availability().rows().next().is_none());
        let delta = captured
            .source_support
            .append_program_fragment(&incoming)
            .unwrap();
        assert!(
            delta.changed_owners.is_empty(),
            "the TYPE binding is unchanged"
        );
        assert!(
            delta
                .support
                .native_availability()
                .same_acquisition(captured.source_support.native_availability()),
            "only offer custody changes; normalized row ownership stays exact"
        );
        assert_eq!(delta.support.native_offers().len(), 2);
        assert!(delta.support.native_offers()[0].same_issuance(&offers[old]));
        assert!(delta.support.native_offers()[1].same_issuance(&offers[current]));

        let advanced = captured
            .advance_program_support(&context, &incoming)
            .unwrap();
        let cold = advanced
            .context
            .capture_world_primary_compiler_inputs(producer.sha256())
            .unwrap();
        assert_eq!(
            advanced.inputs.native_selection.candidates.0,
            cold.native_selection.candidates.0
        );
        assert_eq!(
            advanced.inputs.native_selection.availability,
            cold.native_selection.availability
        );
        assert_eq!(
            advanced
                .inputs
                .native_selection
                .candidates
                .require_unique(owner)
                .unwrap(),
            Some(entry.descriptor.id),
            "the new exact Empty offer supplies direct affinity without granting groups"
        );
        let row = advanced
            .inputs
            .native_availability(entry.descriptor.id)
            .unwrap();
        assert_eq!(row.mode, NativeTransportMode::IssuedGroups);
        assert!(row.issued_ordinals().is_empty());
        assert!(!row.permits(7) && !row.permits(9001));
        assert!(advanced
            .inputs
            .artifacts()
            .selected_native_groups()
            .is_empty());
        assert_eq!(
            NativeCompilerTransport::lower_metadata(&advanced.inputs)
                .unwrap()
                .entries,
            NativeCompilerTransport::lower_metadata(&cold)
                .unwrap()
                .entries
        );
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
            let transport =
                NativeInputSelection::issue_checked(&projection, &metadata, &namespace, &selected)
                    .unwrap();
            let available = &transport.availability.0[&entry.descriptor.id];
            assert!(available.permits(9001));
            assert_eq!(available.offers().len(), 2);
            for offer in &offers {
                assert!(available.offers().any(|available| available == offer));
            }
            let support =
                fixture_support_with_issued_offers(projection.clone(), namespace.clone(), selected)
                    .unwrap();
            let row = support.native_availability().rows().next().unwrap();
            assert!(row.permits(7) && row.permits(9001));
            assert_eq!(row.offers().len(), 2);
            let context = Arc::new(ExactDeclarationContext::from_facts(
                DeclarationContextFacts {
                    producer: producer.sha256(),
                    compiler_support: support,
                    lexical: Default::default(),
                    source_scope: Default::default(),
                    template_imports: None,
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
            let mut validation = PackageInterfaceValidation::default();
            let mut selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                    inputs,
                    &validation.inventory,
                )
                .unwrap();
            let physical = crate::declaration_context::CapturedCompilerStage::issue(
                &mut selection,
                inputs.artifacts(),
                Vec::new(),
                &mut validation,
            )
            .unwrap();
            let group = crate::certified_products::original_available_groups(&native)
                .unwrap()
                .find(|group| group.original_ordinal() == 9001)
                .unwrap();
            let binder = group.binders()[0].clone();
            let prepared = prepared_target_fixture(vec![GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            }]);
            let accepted = [AcceptedGlobal {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                owner: ReceiptImportOwner::Source {
                    unit: native.owner().unit.clone(),
                    module: native.owner().module.clone(),
                    module_version: Some(native.owner().module_version.clone()),
                    original_ordinal: 9001,
                    binder,
                },
            }];
            let observed =
                AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory)
                    .unwrap();
            let demanded = inputs.artifacts().inventory().select_native_target(
                physical.physical_cohort(),
                observed,
                None,
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
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
                NativeInputSelection::issue_checked(&projection, &metadata, &namespace, &offers)
                    .unwrap();
            assert!(transport.availability.0[&entry.descriptor.id].permits(9001));
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
            NativeInputSelection::issue_checked(&projection, &no_demand, &full, &offers).unwrap();
        assert_eq!(
            transport.availability.0[&entry.descriptor.id].permits(7),
            other
        );
        assert!(!offers[0].is_full());
    }
    let empty_offer =
        crate::artifact_inventory::IssuedNativeOffer::issue(entry.descriptor.id, &full, None)
            .unwrap();
    let empty = NativeInputSelection::issue_checked(&projection, &no_demand, &full, &[empty_offer])
        .unwrap();
    assert!(!empty.availability.0[&entry.descriptor.id].permits(7));
}

#[test]
fn candidate_availability_preserves_interface_only_with_historical_dependency() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let dependency = product("AvailableDependency", 181);
    let consumer = dependent_product(
        "AvailableConsumer",
        182,
        vec![(3, vec![source_import(&dependency, 7)])],
    );
    let replacement = product("AvailableDependency", 183);
    let recovered = recovered_witness_fixtures(&[dependency, consumer, replacement]);
    let products = recovered
        .iter()
        .map(|row| row.product.clone())
        .collect::<Vec<_>>();
    let entries = products
        .iter()
        .map(|product| {
            Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        products[0].module_interface(),
        products[2].module_interface()
    );
    assert_ne!(entries[0].descriptor.id, entries[2].descriptor.id);
    let inventory = ArtifactInventory::default();
    let issued = inventory
        .admit_shared(&inventory.empty_view(), entries[..2].to_vec())
        .unwrap();
    let complete = fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&entries[..2]).unwrap(),
        issued,
    );
    for (selected_original, dependency_in_roles) in [(false, true), (true, true), (false, false)] {
        let baseline = if selected_original {
            complete.clone()
        } else {
            let mut owners = vec![entries[1].descriptor.owner.clone()];
            if dependency_in_roles {
                owners.push(entries[0].descriptor.owner.clone());
            }
            complete
                .selected_interfaces()
                .unwrap()
                .support_for_owners(owners)
                .unwrap()
                .retain_template_availability(&complete)
                .unwrap()
        };
        if !dependency_in_roles {
            let dependency_interface =
                ArtifactEntry::canonical(products[0].module_interface().unwrap().clone())
                    .descriptor
                    .id;
            assert!(
                baseline.projection().roles().iter().all(|role| {
                    role.interface() != dependency_interface
                        && role.original() != Some(entries[0].descriptor.id)
                }),
                "A is an exact native child of C, outside the current type roles"
            );
        }
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: baseline.clone(),
            lexical: Default::default(),
            source_scope: Default::default(),
            template_imports: None,
        });
        if !dependency_in_roles {
            let owner = &entries[0].descriptor.owner;
            let initial = context
                .capture_world_primary_compiler_inputs(producer.sha256())
                .unwrap();
            assert_eq!(
                initial
                    .native_selection
                    .candidates
                    .require_unique(owner)
                    .unwrap(),
                None
            );
            assert!(initial
                .native_availability(entries[0].descriptor.id)
                .unwrap()
                .permits(7));
            let types = complete
                .selected_interfaces()
                .unwrap()
                .support_for_owners(vec![owner.clone()])
                .unwrap();
            let program_types = types.clone();
            let type_context = Arc::new(context.clone().rebuild(|facts| {
                facts.compiler_support = types;
            }));
            let advanced = initial.with_type_interfaces(&[type_context]).unwrap();
            assert_eq!(advanced.changed_owners, vec![owner.clone()]);
            let cold = advanced
                .context
                .capture_world_primary_compiler_inputs(producer.sha256())
                .unwrap();
            let program = initial
                .advance_program_support(&advanced.context, &program_types)
                .unwrap();
            assert_eq!(
                program.inputs.native_selection.candidates.0,
                cold.native_selection.candidates.0
            );
            assert_eq!(
                program.inputs.native_selection.availability,
                cold.native_selection.availability
            );
            assert_eq!(
                advanced.inputs.native_selection.candidates.0,
                cold.native_selection.candidates.0
            );
            assert_eq!(
                advanced.inputs.native_selection.availability,
                cold.native_selection.availability
            );
            assert_eq!(
                advanced
                    .inputs
                    .native_selection
                    .candidates
                    .require_unique(owner)
                    .unwrap(),
                Some(entries[0].descriptor.id),
                "the new TYPE role exposes only its already issued exact native child"
            );
            assert_eq!(
                NativeCompilerTransport::lower_metadata(&advanced.inputs)
                    .unwrap()
                    .entries,
                NativeCompilerTransport::lower_metadata(&cold)
                    .unwrap()
                    .entries
            );
        }
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("availability-only-does-not-read-manifest"),
            by_owner: BTreeMap::new(),
            contextual_native_availability: vec![products[2].clone()],
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let input = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .expect("available B cannot compete with the incoming compiler roles")
            .unwrap();
        assert_eq!(
            input.role_projection(),
            baseline.projection(),
            "custody cannot select B"
        );
        assert_eq!(
            input.native_availability(),
            baseline.native_availability(),
            "custody cannot issue Full B"
        );
        assert_eq!(
            input.issued_offers(),
            baseline.native_offers(),
            "custody cannot add an offer for B"
        );
        assert_eq!(
            input
                .issued_input_decisions()
                .map(|decisions| decisions.recorded_facts()),
            Some(baseline.input_decisions().recorded_facts())
        );
        assert!(input
            .issued_artifacts()
            .artifact_ids()
            .contains(&entries[2].descriptor.id));
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context))
            .prepare_compilation_with_private_input(directory.path(), PRODUCER, None, Some(input))
            .expect("the historical consumer remains usable beside available B");
        let inputs = request.compiler_inputs();
        assert_eq!(inputs.projection(), baseline.projection());
        assert!(inputs
            .native_availability(entries[2].descriptor.id)
            .is_none());
        assert!(inputs
            .native_availability(entries[0].descriptor.id)
            .unwrap()
            .permits(7));
        assert!(inputs.artifacts().binding_dependencies().iter().any(|(from, to, edge)| {
            matches!((from, to, edge),
                (crate::artifact_inventory::ArtifactBindingNode::Group(from),
                 crate::artifact_inventory::ArtifactBindingNode::Group(to),
                 crate::artifact_inventory::ArtifactDependency::NativeGroup { .. })
                if from.binding.artifact() == entries[1].descriptor.id
                    && from.original_ordinal == 3 && to.binding.artifact() == entries[0].descriptor.id
                    && to.original_ordinal == 7)
        }));
        let availability = inputs.private_native_availability();
        let dependency_owner = products[0].owner();
        let advertised_dependency = inputs
            .advertised_native_originals()
            .unwrap()
            .receipt_original(
                &availability,
                &dependency_owner.unit,
                &dependency_owner.module,
                &dependency_owner.module_version,
            )
            .unwrap();
        assert_eq!(
            advertised_dependency.is_some(),
            !selected_original && dependency_in_roles,
            "private advertisements require a current interface without a compiler Original: selected_original={selected_original}, dependency_in_roles={dependency_in_roles}"
        );
        assert_eq!(
            inputs
                .projection()
                .roles()
                .iter()
                .any(|role| { role.original() == Some(entries[0].descriptor.id) }),
            selected_original,
            "the actual selected compiler role retains A: selected_original={selected_original}"
        );
        let available_owner = products[2].owner();
        assert!(
            inputs
                .advertised_native_originals()
                .unwrap()
                .receipt_original(
                    &availability,
                    &available_owner.unit,
                    &available_owner.module,
                    &available_owner.module_version,
                )
                .unwrap()
                .is_none(),
            "available B must not become a direct native advertisement"
        );
    }
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
        let prior_projection = CompilerInputProjection::from_issued_entries(&entries[..2]).unwrap();
        let prior_offers = entries[..2]
            .iter()
            .map(|entry| {
                let proof =
                    crate::artifact_inventory::NativeOriginalRequirement::from_selected_view(
                        entry.descriptor.id,
                        &issued,
                    )
                    .unwrap()
                    .unwrap();
                crate::artifact_inventory::IssuedNativeOffer::issue(
                    entry.descriptor.id,
                    &issued,
                    Some(proof),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let prior =
            RequestNativeAvailability::issue_checked(&prior_projection, &issued, &prior_offers)
                .unwrap();
        let current_projection =
            CompilerInputProjection::from_issued_entries(&[entries[2].clone()]).unwrap();
        let current_proof =
            crate::artifact_inventory::NativeOriginalRequirement::from_selected_view(
                entries[2].descriptor.id,
                &current,
            )
            .unwrap()
            .unwrap();
        let current_offer = crate::artifact_inventory::IssuedNativeOffer::issue(
            entries[2].descriptor.id,
            &current,
            Some(current_proof),
        )
        .unwrap();
        let current_rows = RequestNativeAvailability::issue_checked(
            &current_projection,
            &current,
            std::slice::from_ref(&current_offer),
        )
        .unwrap();
        let normalized = prior.merge(&current_rows).unwrap().into_rows();
        let filtered = normalized.for_compiler_roles(&projection, &view).unwrap();
        let required = filtered
            .rows()
            .find(|row| row.original.descriptor.id == entries[0].descriptor.id)
            .expect("the surviving consumer still requires its historical child");
        assert!(required.permits(7));
        assert!(!required.permits(9001));
        let mut offers = prior_offers;
        offers.push(current_offer);
        let receive_error =
            NativeInputSelection::receive(&projection, &metadata, &view, &normalized, &offers)
                .err()
                .expect(
                    "a required historical child cannot bypass an explicit conflicting Original",
                );
        assert_eq!(
            receive_error.to_string(),
            "extractor contract failure: artifact inventory: issued native dependency conflicts with request interface"
        );
        let error = NativeInputSelection::issue_checked(
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
fn candidate_availability_keeps_partial_same_owner_input_permissions() {
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
    let root = tempfile::tempdir().unwrap();
    for (index, availability) in [
        vec![baseline.clone()],
        vec![baseline.clone(), unused.clone()],
    ]
    .into_iter()
    .enumerate()
    {
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("partial-input-availability"),
            by_owner: BTreeMap::new(),
            contextual_native_availability: availability,
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let input = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .unwrap()
            .unwrap();
        assert_eq!(input.role_projection(), context.compiler_input_projection());
        assert_eq!(
            input.native_availability(),
            context.compiler_source_support().native_availability()
        );
        assert_eq!(
            input.issued_offers(),
            context.compiler_source_support().native_offers()
        );
        let request = ExactCompileContext::new(context.clone())
            .prepare_compilation_with_private_input(
                &root.path().join(index.to_string()),
                PRODUCER,
                None,
                Some(input),
            )
            .unwrap();
        let rows = wire_groups(&request);
        assert_eq!(rows["BaselineNative"], vec![7]);
        assert_eq!(wire_mode(&request, "BaselineNative"), issued_mode(&[7]));
        assert!(!rows.contains_key("UnusedNative"));
        assert_eq!(context.as_ref(), &before);
        assert_eq!(
            request
                .compiler_inputs()
                .metadata
                .retained
                .selected_native_groups,
            before
                .compiler_source_support()
                .artifacts()
                .metadata_snapshot()
                .selected_native_groups
        );
    }
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
    let authored = |version| {
        crate::certified_products::fixture_native_declaration_product(
            crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
                "main",
                &module,
                vec![(7, vec![source_import(&helper, 7)]), (9001, vec![])],
                version,
                &BTreeMap::new(),
                b"issued authored interface".to_vec(),
                &BTreeMap::new(),
                &[],
            ),
            producer.sha256(),
            1,
        )
    };
    let available_authored = recovered_witness_fixtures(&[authored(75)])
        .remove(0)
        .product;
    let recovered = recovered_witness_fixtures(&[authored(74), helper]);
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
        .unwrap();
    let narrowed = fixture_support_with_issued_offers(
        narrowed.projection().clone(),
        narrowed.artifacts().clone(),
        [offer],
    )
    .unwrap();
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: narrowed,
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            source_scope: Default::default(),
            template_imports: None,
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
    let input = OriginalCompilerInputs::reuse_selected_authored(&context)
        .expect("private offer preserves its already-issued exact dependency closure")
        .expect("authored body is privately reusable");
    let available_authored_id =
        ArtifactEntry::original(producer.sha256(), available_authored.clone())
            .unwrap()
            .descriptor
            .id;
    assert_ne!(available_authored_id, authored_id);
    assert_eq!(
        available_authored.module_interface(),
        recovered
            .iter()
            .find(|row| row.product.owner().module == module)
            .unwrap()
            .product
            .module_interface()
    );
    let unrelated = product("UnrelatedAuthoredAvailability", 184);
    let unrelated_id = ArtifactEntry::original(producer.sha256(), unrelated.clone())
        .unwrap()
        .descriptor
        .id;
    for products in [Vec::new(), vec![unrelated.clone()]] {
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("authored-reuse-custody-only"),
            by_owner: BTreeMap::new(),
            contextual_native_availability: products,
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let received = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .unwrap()
            .expect("authored availability is independent of optional catalog custody");
        assert_eq!(received.role_projection(), input.role_projection());
        assert_eq!(received.native_availability(), input.native_availability());
        assert_eq!(received.issued_offers(), input.issued_offers());
        assert_eq!(
            received
                .issued_input_decisions()
                .map(|decisions| decisions.recorded_facts()),
            input
                .issued_input_decisions()
                .map(|decisions| decisions.recorded_facts())
        );
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(context.clone())
            .prepare_compilation_with_private_input(
                directory.path(),
                PRODUCER,
                None,
                Some(received),
            )
            .unwrap();
        assert!(request
            .compiler_inputs()
            .native_availability(unrelated_id)
            .is_none());
        assert_eq!(wire_groups(&request)[&module], Vec::<u32>::new());
        assert_eq!(wire_groups(&request)["TypeOnlyHelper"], Vec::<u32>::new());
    }
    let mut history = context.as_ref().clone();
    for (step, products) in [
        vec![available_authored.clone()],
        Vec::new(),
        vec![available_authored],
        Vec::new(),
    ]
    .into_iter()
    .enumerate()
    {
        let available = !products.is_empty();
        let source_available = history
            .artifact_view()
            .artifact_ids()
            .contains(&available_authored_id);
        eprintln!("authored_custody_history stage=before_fallback step={step} candidate_empty={} source_available={source_available}", !available);
        if step > 0 {
            assert!(
                source_available,
                "the receiving context retains B before step={step} available={available}"
            );
        }
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("retained-authored-availability-history"),
            by_owner: BTreeMap::new(),
            contextual_native_availability: products,
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let received = OriginalCompilerInputs::from_candidates(&history, producer, &candidates)
            .unwrap()
            .expect("the issued authored fallback remains available");
        eprintln!("authored_custody_history stage=after_fallback step={step} candidate_empty={} retained_available={}",
            !available, received.issued_artifacts().artifact_ids().contains(&available_authored_id));
        assert_eq!(received.role_projection(), input.role_projection());
        assert_eq!(received.native_availability(), input.native_availability());
        assert_eq!(received.issued_offers(), input.issued_offers());
        assert_eq!(
            received
                .issued_input_decisions()
                .map(|decisions| decisions.recorded_facts()),
            input
                .issued_input_decisions()
                .map(|decisions| decisions.recorded_facts())
        );
        if available {
            assert!(
                received
                    .issued_artifacts()
                    .artifact_ids()
                    .contains(&available_authored_id),
                "nonempty receipt acquires B custody at step={step}"
            );
        }
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(history.clone()))
            .prepare_compilation_with_private_input(
                directory.path(),
                PRODUCER,
                None,
                Some(received),
            )
            .unwrap();
        assert!(request
            .compiler_inputs()
            .native_availability(available_authored_id)
            .is_none());
        assert_eq!(wire_groups(&request)[&module], Vec::<u32>::new());
        assert!(
            request
                .compiler_inputs()
                .artifacts()
                .binding_dependencies()
                .iter()
                .any(|(from, to, edge)| {
                    matches!((from, to, edge),
                (crate::artifact_inventory::ArtifactBindingNode::Group(from),
                 crate::artifact_inventory::ArtifactBindingNode::Group(to),
                 crate::artifact_inventory::ArtifactDependency::NativeGroup { .. })
                if from.binding.artifact() == authored_id && from.original_ordinal == 7
                    && to.binding.artifact() == helper_id && to.original_ordinal == 7)
                }),
            "authored closure retains its exact required helper group at step={step}"
        );
        let retained = history
            .compiler_source_support()
            .retain_custody(request.compiler_inputs().artifacts())
            .unwrap();
        assert!(retained
            .artifacts()
            .artifact_ids()
            .contains(&available_authored_id));
        assert_eq!(retained.projection(), context.compiler_input_projection());
        assert_eq!(
            retained.native_offers(),
            context.compiler_source_support().native_offers()
        );
        history = history.rebuild(|facts| facts.compiler_support = retained);
    }
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
    let helper_artifact = request
        .artifacts
        .iter()
        .find(|artifact| artifact.interface.module == "TypeOnlyHelper")
        .unwrap();
    assert!(
        helper_artifact.product.is_some(),
        "the issued-groups row keeps its transported native product anchor"
    );
    request
        .validate_artifacts()
        .expect("issued-groups transport row keeps a valid native anchor");
    assert!(!request.input_transport_files().is_empty());
    let availability = request
        .compiler_inputs()
        .native_availability(authored_id)
        .unwrap();
    let retained = availability
        .offers()
        .filter(|offer| offer.original() == authored_id)
        .find_map(|offer| offer.proof())
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
        .extend_issued_compiler_support(&{
            let entry =
                Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
            let inventory = ArtifactInventory::default();
            let artifacts = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    vec![entry.clone()],
                    crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
            let mut validation = PackageInterfaceValidation::default();
            let mut selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &projection,
                    &artifacts.metadata_snapshot(),
                    &validation.inventory,
                )
                .unwrap();
            let output = CapturedCompilerStage::issue(
                &mut selection,
                &artifacts,
                Vec::new(),
                &mut validation,
            )
            .unwrap()
            .compiler_inputs()
            .as_ref()
            .clone();
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&output)
                .unwrap()
        })
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
    let original = ArtifactEntry::original(producer.sha256(), native.clone())
        .unwrap()
        .descriptor
        .id;
    let full = context
        .compiler_source_support()
        .native_offers()
        .iter()
        .find(|offer| offer.original() == original && offer.is_full())
        .expect("the type-only source owner retains an actual complete issued offer");
    let issued = full
        .issued_group_carriers()
        .unwrap()
        .into_keys()
        .filter(|group| group.binding.artifact() == original)
        .map(|group| group.original_ordinal)
        .collect::<BTreeSet<_>>();
    assert_eq!(issued, BTreeSet::from([7, 9001]));
    assert_eq!(
        crate::certified_products::original_available_groups(&native)
            .unwrap()
            .map(|group| group.original_ordinal())
            .collect::<BTreeSet<_>>(),
        issued,
        "opaque Full permission and the independent immutable body have the same census"
    );
    assert!(OriginalCompilerInputs::reuse_selected_authored(&context)
        .unwrap()
        .is_none());
    // A separately issued configured offer remains legitimate. Full custody
    // and a session-like module name cannot issue that offer implicitly.
    assert!((|| -> Result<OriginalCompilerInputs, CompileError> {
        let producer = producer;
        let entries = (std::slice::from_ref(&native))
            .iter()
            .map(|product| {
                ArtifactEntry::original(producer.sha256(), product.clone()).map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let inventory = ArtifactInventory::default();
        let artifacts = inventory.admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )?;
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
        CapturedCompilerStage::issue(&mut selection, &artifacts, Vec::new(), &mut validation)
            .map(|stage| stage.compiler_inputs().as_ref().clone())
    })()
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
    let input = (|| -> Result<OriginalCompilerInputs, CompileError> {
        let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
        let entries = (std::slice::from_ref(&native))
            .iter()
            .map(|product| {
                ArtifactEntry::original(producer.sha256(), product.clone()).map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let inventory = ArtifactInventory::default();
        let artifacts = inventory.admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )?;
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
        CapturedCompilerStage::issue(&mut selection, &artifacts, Vec::new(), &mut validation)
            .map(|stage| stage.compiler_inputs().as_ref().clone())
    })()
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
fn native_availability_preserves_ambiguity_and_explicit_selection_refuses_conflicts() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
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
    let candidates = crate::module_candidates::CandidateSet {
        manifest_path: PathBuf::from("ambiguous-availability-only"),
        by_owner: BTreeMap::new(),
        contextual_native_availability: vec![first.clone()],
        catalog_owner: None,
        input_transport: Vec::new(),
    };
    let owner = identity(first.owner().unit.as_str(), first.owner().module.as_str());
    assert!(
        matches!(OriginalCompilerInputs::from_candidates(&changed, producer, &candidates),
        Err(CompileError::ArtifactInventory(error))
            if matches!(&error.failure, ArtifactInventoryFailure::OwnerConflict { owner: actual } if actual == &owner))
    );
    let entries = [&first, &second]
        .into_iter()
        .map(|product| {
            Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    let mut ambiguous = context.as_ref().clone();
    let inventory = ambiguous.artifact_view().inventory().clone();
    let custody = inventory
        .admit_shared_with_demand(
            ambiguous.artifact_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let support = fixture_compiler_support(ambiguous.compiler_input_projection().clone(), custody);
    ambiguous = ambiguous.rebuild(|facts| facts.compiler_support = support);
    assert!(ambiguous
        .artifact_view()
        .metadata_snapshot()
        .ambiguous_native_owners
        .contains(&owner));
    assert!(ambiguous
        .artifact_view()
        .selected_native_groups()
        .is_empty());
    let received = OriginalCompilerInputs::from_candidates(&ambiguous, producer, &candidates)
        .unwrap()
        .unwrap();
    assert_eq!(
        received.role_projection(),
        ambiguous.compiler_input_projection()
    );
    assert_eq!(
        received.native_availability(),
        ambiguous.compiler_source_support().native_availability()
    );
    assert_eq!(
        received.issued_offers(),
        ambiguous.compiler_source_support().native_offers()
    );
    assert!(received
        .role_projection()
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(Arc::new(ambiguous.clone()))
        .prepare_compilation_with_private_input(
            &root.path().join("available"),
            PRODUCER,
            None,
            Some(received),
        )
        .unwrap();
    for entry in &entries {
        if let Some(row) = request
            .compiler_inputs()
            .native_availability(entry.descriptor.id)
        {
            assert!(row.issued_ordinals().is_empty());
            assert_ne!(row.mode, NativeTransportMode::Full);
        }
    }
    assert!(request
        .compiler_inputs()
        .artifacts()
        .selected_native_groups()
        .is_empty());

    // An independently issued A selection remains exact while B is custody.
    let selected = inventory
        .admit_shared(&inventory.empty_view(), vec![entries[0].clone()])
        .unwrap()
        .retain_custody(ambiguous.artifact_view())
        .unwrap();
    let first_id = entries[0].descriptor.id;
    let second_id = entries[1].descriptor.id;
    assert_ne!(first_id, second_id);
    assert_eq!(
        selected
            .selected_native_groups()
            .iter()
            .map(|group| group.artifact)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_id])
    );
    assert!(selected.artifact_ids().contains(&second_id));
    let projection = CompilerInputProjection::from_issued_entries(&[entries[0].clone()]).unwrap();
    let support = fixture_compiler_support(projection.clone(), selected);
    ambiguous = ambiguous.rebuild(|facts| facts.compiler_support = support);
    let explicit_a =
        OriginalCompilerInputs::from_issued_support(ambiguous.compiler_source_support());
    let accepted = ExactCompileContext::new(Arc::new(ambiguous.clone()))
        .prepare_compilation_with_private_input(
            &root.path().join("selected-a"),
            PRODUCER,
            None,
            Some(explicit_a),
        )
        .unwrap();
    assert_eq!(accepted.compiler_inputs().projection(), &projection);
    assert!(accepted
        .compiler_inputs()
        .native_availability(first_id)
        .unwrap()
        .permits(7));
    assert!(accepted
        .compiler_inputs()
        .native_availability(second_id)
        .is_none());
    let output_inventory = ArtifactInventory::default();
    let physical = output_inventory
        .admit_shared_with_demand(
            &output_inventory.empty_view(),
            vec![entries[1].clone()],
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let projection_b = CompilerInputProjection::from_issued_entries(&[entries[1].clone()]).unwrap();
    let mut validation = PackageInterfaceValidation::default();
    let mut selection_b =
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &projection_b,
            &physical.metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
    let stage =
        CapturedCompilerStage::issue(&mut selection_b, &physical, Vec::new(), &mut validation)
            .unwrap();
    let explicit_b = stage.compiler_inputs().as_ref().clone();
    assert!(matches!(ExactCompileContext::new(Arc::new(ambiguous))
        .prepare_compilation_with_private_input(&root.path().join("selected-b"), PRODUCER, None, Some(explicit_b)),
        Err(CompileError::ArtifactInventory(error))
            if matches!(&error.failure, ArtifactInventoryFailure::CompilerOriginalOfferConflict { owner: actual, existing, incoming }
                if actual == &owner && *existing == first_id && *incoming == second_id)));
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
        let issued = fixture_support_with_issued_offers(
            projection.clone(),
            selected.clone(),
            [offer.clone()],
        );
        // Checked issuance supplies the normalized permission rows that
        // activation request preparation requires; raw recovery custody alone
        // carries none.
        let support = issued.unwrap();
        let context = ExactDeclarationContext::retained_execution_support_fixture(
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
        let inputs = OriginalCompilerInputs::reuse_selected_authored(&context)
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
            OriginalCompilerInputs::reuse_selected_authored(&context)
                .unwrap()
                .is_none(),
            "body custody alone does not grant a selected authored root"
        );
        let availability = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("authored-winner-availability"),
            by_owner: BTreeMap::new(),
            contextual_native_availability: vec![current.clone()],
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let received = OriginalCompilerInputs::from_candidates(&context, producer, &availability)
            .unwrap()
            .unwrap();
        assert_eq!(
            received.role_projection(),
            context.compiler_input_projection()
        );
        assert!(received
            .role_projection()
            .roles()
            .iter()
            .all(|role| role.original().is_none()));
        assert!(received.native_availability().rows().next().is_none());
        assert!(received.issued_offers().is_empty());
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
        let received = OriginalCompilerInputs::from_candidates(&unissued, producer, &availability)
            .unwrap()
            .unwrap();
        assert_eq!(
            received.role_projection(),
            unissued.compiler_input_projection()
        );
        assert!(received.role_projection().roles().is_empty());
        assert!(received.native_availability().rows().next().is_none());
        assert!(received.issued_offers().is_empty());
        let available_context = context.clone();
        let physical_a = context
            .artifact_view()
            .inventory()
            .admit_shared_with_demand(
                context.artifact_view(),
                vec![entries[0].clone()],
                crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection_a =
            CompilerInputProjection::from_issued_entries(&[entries[0].clone()]).unwrap();
        let mut validation = PackageInterfaceValidation::default();
        let mut selection_a =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection_a,
                &physical_a.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let stage = CapturedCompilerStage::issue(
            &mut selection_a,
            &physical_a,
            Vec::new(),
            &mut validation,
        )
        .unwrap();
        let selected_a = stage.compiler_inputs().as_ref().clone();
        let support_a =
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&selected_a)
                .unwrap();
        context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&support_a)
            .unwrap();
        assert!(context.artifact_view().selected_native_groups().is_empty());
        let private = OriginalCompilerInputs::from_candidates(&context, producer, &availability)
            .unwrap()
            .unwrap();
        assert_eq!(
            private.role_projection(),
            context.compiler_input_projection()
        );
        assert_eq!(
            private.native_availability(),
            context.compiler_source_support().native_availability()
        );
        let available_b = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("authored-other-availability"),
            by_owner: BTreeMap::new(),
            contextual_native_availability: vec![products[1].clone()],
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let competing_native = entries[1].descriptor.id;
        let received_b = OriginalCompilerInputs::from_candidates(&context, producer, &available_b)
            .unwrap()
            .unwrap();
        assert_eq!(received_b.role_projection(), private.role_projection());
        assert_eq!(
            received_b.native_availability(),
            private.native_availability()
        );
        assert_eq!(received_b.issued_offers(), private.issued_offers());
        let physical_b = available_context
            .artifact_view()
            .inventory()
            .admit_shared_with_demand(
                available_context.artifact_view(),
                vec![entries[1].clone()],
                crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection_b =
            CompilerInputProjection::from_issued_entries(&[entries[1].clone()]).unwrap();
        let mut validation = PackageInterfaceValidation::default();
        let mut selection_b =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection_b,
                &physical_b.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let stage = CapturedCompilerStage::issue(
            &mut selection_b,
            &physical_b,
            Vec::new(),
            &mut validation,
        )
        .unwrap();
        let other = stage.compiler_inputs().as_ref().clone();
        let conflict_root = tempfile::tempdir().unwrap();
        assert!(matches!(ExactCompileContext::new(Arc::new(context.clone()))
            .prepare_compilation_with_private_input(conflict_root.path(), PRODUCER, None, Some(other.clone())),
            Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                ArtifactInventoryFailure::CompilerOriginalOfferConflict { existing, incoming, .. }
                    if existing == current_native && incoming == competing_native)));
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
                .selected_native_groups
                .as_ref()
                .clone(),
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
            .unwrap()
            .retain_normalized_availability(context.compiler_source_support().native_availability())
            .unwrap();
            assert_eq!(
                support.native_availability(),
                context.compiler_source_support().native_availability(),
                "recovered graph census must retain the actual selected output permission row"
            );
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
        let private = OriginalCompilerInputs::reuse_selected_authored(&context)
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
        let exact = OriginalCompilerInputs::reuse_selected_authored(&canonical_only)
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
        let received_b =
            OriginalCompilerInputs::from_candidates(&canonical_only, producer, &available_b)
                .unwrap()
                .unwrap();
        assert_eq!(received_b.role_projection(), exact.role_projection());
        assert!(
            matches!(exact.merge(&other), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::CompilerOriginalOfferConflict { existing, incoming, .. }
                if (existing == current_native && incoming == competing_native)
                    || (incoming == current_native && existing == competing_native)))
        );
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
            let mixed =
                OriginalCompilerInputs::from_candidates(&historical, producer, &availability)
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
            let error = OriginalCompilerInputs::reuse_selected_authored(&competing).unwrap_err();
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
            package_interfaces: wrong
                .original_native()
                .unwrap()
                .package_interfaces()
                .clone(),
            execution_source: None,
        },
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let context = type_context(&[wrong.clone(), required.clone()]);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let valid = dependent_product(
        "DependentNative",
        32,
        vec![(7, vec![source_import(&required, 7)])],
    );
    let valid = recovered_witness_fixtures(&[valid, required.clone()])
        .into_iter()
        .find(|row| row.product.owner().module == "DependentNative")
        .unwrap()
        .product;
    for (index, output, wrong_ordinal) in [(0, valid, false), (1, wrong, true)] {
        let entries = [output, required.clone()]
            .into_iter()
            .map(|product| Arc::new(ArtifactEntry::original(producer.sha256(), product).unwrap()))
            .collect::<Vec<_>>();
        let required_id = entries[1].descriptor.id;
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let inventory = ArtifactInventory::default();
        let physical = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries,
                crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &physical.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let selected =
            CapturedCompilerStage::issue(&mut selection, &physical, Vec::new(), &mut validation)
                .map(|stage| stage.compiler_inputs().as_ref().clone());
        if wrong_ordinal {
            assert!(matches!(selected,
                Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                    ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal: 999 }
                        if artifact == required_id)));
        } else {
            let root = tempfile::tempdir().unwrap();
            let request = ExactCompileContext::new(context.clone())
                .prepare_compilation_with_private_input(
                    &root.path().join(index.to_string()),
                    PRODUCER,
                    None,
                    Some(selected.unwrap()),
                )
                .unwrap();
            assert_eq!(wire_mode(&request, "DependentNative"), full_mode());
            assert_eq!(wire_mode(&request, "RequiredNative"), full_mode());
        }
    }
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
    // Compiler roles and the declaration's issued SOURCE/lexical scope are
    // independent. This demotion changes only the former.
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
    let assert_exact_offer = |context: &ExactDeclarationContext,
                              name: &str,
                              expected: Option<Value>| {
        let before = context.clone();
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
        if let Some(mode) = expected {
            let [row] = offered.as_slice() else {
                panic!("the selected authored original must have one advertised native row: {offered:?}");
            };
            assert_eq!(row[8], mode);
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
                "TYPE evidence alone cannot advertise an Original"
            );
        }
        assert_eq!(context, &before);
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
    let native = OriginalCompilerInputs::reuse_selected_authored(&native_context)
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
    assert!(native_request
        .compiler_inputs()
        .metadata
        .retained
        .selected_native_groups
        .is_empty());
    let operation = tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
    let available = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        native_request.compiler_inputs(),
        &operation,
    )
    .unwrap();
    for group in &authored_groups {
        assert!(available.permits_native_group(
            authored_root,
            certificate.product().owner(),
            group.original_ordinal,
        ));
        assert!(
            crate::certified_products::original_native_group_definitions(
                certificate.product(),
                group.original_ordinal,
            )
            .is_some()
        );
    }
    let absent_ordinal = authored_groups
        .iter()
        .map(|group| group.original_ordinal)
        .max()
        .unwrap()
        .checked_add(1)
        .unwrap();
    // Full native permission does not assert that an ordinal has a body.
    // Authenticate body membership and issued candidates independently.
    assert!(
        crate::certified_products::original_native_group_definitions(
            certificate.product(),
            absent_ordinal,
        )
        .is_none()
    );
    assert!(available
        .native_group_candidates(
            crate::artifact_inventory::NativeGroupKey {
                artifact: authored_root,
                original_ordinal: absent_ordinal,
            },
            certificate.product().owner(),
        )
        .unwrap()
        .bindings()
        .is_empty());
    assert_exact_offer(
        &native_context,
        "full-authored-exact-offer",
        Some(full_mode()),
    );
    // Projecting compiler support keeps issued native availability while
    // dropping current executable roots. The captured source scope is unchanged.
    let mut captured = native_context.clone();
    let permissions = captured
        .compiler_source_support()
        .native_availability()
        .snapshot_facts()
        .unwrap();
    let support = captured
        .compiler_source_support()
        .source_support(
            &captured
                .compiler_input_projection()
                .snapshot_roles()
                .keys()
                .cloned()
                .collect(),
        )
        .unwrap();
    assert_eq!(
        support.native_availability().snapshot_facts().unwrap(),
        permissions
    );
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
    let private = OriginalCompilerInputs::reuse_selected_authored(&captured)
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
    assert_exact_offer(
        &captured,
        "custodied-authored-exact-offer",
        Some(full_mode()),
    );
    assert_eq!(captured, before_custody);
    // A fresh receiver gains TYPE evidence without the source's native owner.
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
    assert_eq!(
        fresh.compiler_input_projection(),
        native_context.compiler_input_projection()
    );
    assert!(fresh.source_scope().rows().is_empty());
    assert!(fresh.lexical_graph().is_empty());
    assert!(fresh.compiler_source_support().native_offers().is_empty());
    assert!(fresh
        .compiler_source_support()
        .native_availability()
        .snapshot_facts()
        .unwrap()
        .is_empty());
    assert!(OriginalCompilerInputs::reuse_selected_authored(&fresh)
        .unwrap()
        .is_none());
    assert_exact_offer(&fresh, "fresh-request-type-authored-offer", None);

    // The same TYPE extension preserves availability already issued to its
    // receiver without selecting any executable group.
    let retained = captured
        .clone()
        .extend_interface_context(&native_context)
        .unwrap();
    assert_eq!(
        retained
            .compiler_source_support()
            .native_availability()
            .snapshot_facts()
            .unwrap(),
        captured
            .compiler_source_support()
            .native_availability()
            .snapshot_facts()
            .unwrap()
    );
    assert!(retained.artifact_view().selected_native_groups().is_empty());
    assert_eq!(retained.lexical_graph(), captured.lexical_graph());
    assert_eq!(retained.source_scope(), captured.source_scope());
    assert_exact_offer(
        &retained,
        "retained-request-type-authored-offer",
        Some(full_mode()),
    );
    let full_data = ExactDeclarationContext::new(&[], &[], Vec::new())
        .unwrap()
        .extend_issued_compiler_support(captured.compiler_source_support())
        .unwrap();
    assert!(full_data.source_scope().rows().is_empty());
    assert!(full_data.lexical_graph().is_empty());
    assert!(full_data
        .artifact_view()
        .selected_native_groups()
        .is_empty());
    assert_exact_offer(
        &full_data,
        "full-data-authored-exact-offer",
        Some(full_mode()),
    );

    // Recovered DATA fixtures issue only the literal selected group subset.
    // Their Original role identifies bytes; their normalized row bounds which
    // bodies can be offered. They carry no authored SOURCE or lexical grant.
    let mut valid_sparse_permissions = 0;
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
        let view = native_context
            .owner
            .facts
            .compiler_support
            .artifacts()
            .inventory()
            .admit_recovery_selection(
                &native_context.artifact_view().inventory().empty_view(),
                native_context.artifact_view().entries(),
                &selected,
            );
        let view = match view {
            Ok(view) => view,
            Err(error) => {
                assert!(
                    omitted.is_some(),
                    "empty DATA selection must remain valid: {error:?}"
                );
                assert!(
                    matches!(&error, CompileError::ExtractFailed(detail)
                        if detail == "artifact inventory: recovered native group closure differs from certified demand"),
                    "only a required omitted group may refuse the narrowed DATA closure: {error:?}"
                );
                continue;
            }
        };
        let native_roles = CompilerInputProjection::from_issued_entries(&view.entries()).unwrap();
        let support = fixture_compiler_support(native_roles, view)
            .for_source_owners(&BTreeSet::new())
            .unwrap();
        let partial = ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .extend_issued_compiler_support(&support)
            .unwrap();
        assert!(partial.source_scope().rows().is_empty());
        assert!(partial.lexical_graph().is_empty());
        assert!(partial.owner.facts.template_imports.is_none());
        assert!(
            partial.artifact_view().selected_native_groups().is_empty(),
            "extending retained support transfers permission without executable demand"
        );
        let expected_ordinals = selected
            .iter()
            .filter(|group| group.artifact == authored_root)
            .map(|group| group.original_ordinal)
            .collect::<BTreeSet<_>>();
        let private = OriginalCompilerInputs::reuse_selected_authored(&partial)
            .unwrap()
            .expect("bounded native availability retains its exact Original identity");
        assert_eq!(
            private.projection.snapshot_roles()[&identity(&owner.unit, &owner.module)].original(),
            Some(authored_root)
        );
        let before = partial.clone();
        let request = ExactCompileContext::new(Arc::new(partial.clone()))
            .prepare_compilation_with_private_input(
                &root.join(format!("partial-authored-input-{selection_index}")),
                producer_bytes,
                None,
                Some(private),
            )
            .unwrap();
        let inputs = request.compiler_inputs();
        let row = inputs.native_availability(authored_root).unwrap();
        assert_eq!(row.original_owner(), certificate.product().owner());
        assert_eq!(row.mode, NativeTransportMode::IssuedGroups);
        assert_eq!(row.issued_ordinals(), expected_ordinals);
        assert!(inputs.artifacts().selected_native_groups().is_empty());
        assert!(request.groups.is_empty());
        assert_eq!(request.context().as_ref(), &before);
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            inputs, &operation,
        )
        .unwrap();
        for group in &authored_groups {
            let permitted = expected_ordinals.contains(&group.original_ordinal);
            assert_eq!(row.permits(group.original_ordinal), permitted);
            assert_eq!(
                selection.permits_native_group(
                    authored_root,
                    certificate.product().owner(),
                    group.original_ordinal,
                ),
                permitted
            );
            assert!(
                crate::certified_products::original_native_group_definitions(
                    certificate.product(),
                    group.original_ordinal,
                )
                .is_some()
            );
            if !permitted {
                assert!(selection
                    .native_group_candidates(
                        crate::artifact_inventory::NativeGroupKey {
                            artifact: authored_root,
                            original_ordinal: group.original_ordinal,
                        },
                        certificate.product().owner(),
                    )
                    .unwrap()
                    .bindings()
                    .is_empty());
            }
        }
        assert_exact_offer(
            &partial,
            &format!("partial-authored-exact-offer-{selection_index}"),
            Some(issued_mode(
                &expected_ordinals.iter().copied().collect::<Vec<_>>(),
            )),
        );
        if !expected_ordinals.is_empty() {
            valid_sparse_permissions += 1;
        }
    }
    assert!(
        valid_sparse_permissions > 0,
        "a valid nonempty sparse selection must retain bounded permission without Full"
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
    let original = entry.descriptor.id;
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
            source_scope: Default::default(),
            template_imports: None,
        },
    ));
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context)
        .prepare_compilation(root.path(), PRODUCER)
        .unwrap();
    let inputs = request.compiler_inputs();
    let issued = inputs.native_availability(original).unwrap();
    assert!(matches!(&issued.mode, NativeTransportMode::IssuedGroups));
    assert!(issued.issued_ordinals().is_empty());
    assert_eq!(
        issued.classification,
        NativeInputClassification::CompilerRole
    );
    assert!(inputs
        .projection()
        .roles()
        .iter()
        .any(|role| role.original() == Some(original)));
    assert_eq!(wire_mode(&request, "UnavailableOriginal"), issued_mode(&[]));
    assert_eq!(
        wire_classification(&request, "UnavailableOriginal"),
        text("compiler-original")
    );
    assert!(inputs.artifacts().selected_native_groups().is_empty());
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
        let custody = selected.retain_custody(&full).unwrap();
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
        let support =
            fixture_support_with_issued_offers(projection.clone(), custody.clone(), [offer])
                .unwrap();
        let private = OriginalCompilerInputs::from_issued_support(&support);
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
                source_scope: Default::default(),
                template_imports: None,
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
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                inputs,
                &validation.inventory,
            )
            .unwrap();
        let permissions = [7, 9001].map(|ordinal| {
            selection.permits_native_group(entry.descriptor.id, native.owner(), ordinal)
        });
        // This target has no fresh native products. Empty physical issuance
        // records that cohort without changing its request's input permissions.
        let physical = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            inputs.artifacts(),
            Vec::new(),
            &mut validation,
        )
        .unwrap();
        assert_eq!(
            [7, 9001].map(|ordinal| {
                selection.permits_native_group(entry.descriptor.id, native.owner(), ordinal)
            }),
            permissions,
        );
        for demanded in [7, 9001] {
            let group = crate::certified_products::original_available_groups(&native)
                .unwrap()
                .find(|group| group.original_ordinal() == demanded)
                .unwrap();
            let binder = group.binders()[0].clone();
            let prepared = prepared_target_fixture(vec![GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            }]);
            let accepted = [AcceptedGlobal {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                owner: ReceiptImportOwner::Source {
                    unit: native.owner().unit.clone(),
                    module: native.owner().module.clone(),
                    module_version: Some(native.owner().module_version.clone()),
                    original_ordinal: demanded,
                    binder,
                },
            }];
            let observed =
                AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory)
                    .unwrap();
            let admitted = inputs.artifacts().inventory().select_native_target(
                physical.physical_cohort(),
                observed,
                None,
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
            );
            let admitted = admitted.map(|_| ());
            if ordinal == Some(demanded) {
                assert!(
                    admitted.is_ok(),
                    "offered={ordinal:?}, demanded={demanded}, actual={admitted:?}"
                );
            } else {
                assert!(
                    matches!(&admitted, Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal }
                        if artifact == entry.descriptor.id && original_ordinal == demanded)),
                    "offer={ordinal:?}, demand={demanded}, actual={admitted:?}"
                );
            }
        }
        let group = crate::certified_products::original_available_groups(&unrelated)
            .unwrap()
            .find(|group| group.original_ordinal() == 7)
            .unwrap();
        let binder = group.binders()[0].clone();
        let prepared = prepared_target_fixture(vec![GlobalDecl {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        }]);
        let accepted = [AcceptedGlobal {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Source {
                unit: unrelated.owner().unit.clone(),
                module: unrelated.owner().module.clone(),
                module_version: Some(unrelated.owner().module_version.clone()),
                original_ordinal: 7,
                binder,
            },
        }];
        let observed =
            AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory).unwrap();
        let denied = inputs.artifacts().inventory().select_native_target(
            physical.physical_cohort(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            &mut validation,
        );
        assert!(
            matches!(&denied, Err(CompileError::CompilerEvidence(error))
            if matches!(error.as_ref(), crate::certified_products::CertificationError::SourceOutsideCompilerSelection {
                owner, requested_version, original_ordinal: None,
            } if owner.unit == unrelated.owner().unit
                && owner.module == unrelated.owner().module
                && requested_version == &unrelated.owner().module_version)),
            "unoffered dependency custody cannot manufacture a Source owner; actual={denied:?}"
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
            native_refusal: None,
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
                compiler_support: fixture_support_with_issued_offers(
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
                source_scope: Default::default(),
                template_imports: None,
            });
            assert_eq!(
                context
                    .compiler_source_support()
                    .native_availability()
                    .snapshot_facts()
                    .unwrap(),
                recompute_native_availability_facts(
                    context.compiler_source_support().native_availability()
                )
            );
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
            assert_eq!(
                restored
                    .compiler_source_support()
                    .native_availability()
                    .snapshot_facts()
                    .unwrap(),
                recompute_native_availability_facts(
                    restored.compiler_source_support().native_availability()
                )
            );
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
            let physical_rows = request.compiler_inputs().private_native_availability();
            assert_eq!(
                physical_rows.snapshot_facts().unwrap(),
                recompute_native_availability_facts(&physical_rows)
            );
            if interface_only && index != 0 {
                assert!(!request
                    .compiler_inputs()
                    .native_selection
                    .availability
                    .0
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
                let mut validation = PackageInterfaceValidation::default();
                let mut selection =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                        inputs,
                        &validation.inventory,
                    )
                    .unwrap();
                let physical = crate::declaration_context::CapturedCompilerStage::issue(
                    &mut selection,
                    inputs.artifacts(),
                    Vec::new(),
                    &mut validation,
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
                    let group = crate::certified_products::original_available_groups(native)
                        .unwrap()
                        .find(|group| group.original_ordinal() == 9001)
                        .unwrap();
                    let binder = group.binders()[0].clone();
                    let prepared = prepared_target_fixture(vec![GlobalDecl {
                        identity: binder.clone(),
                        rep: RuntimeRep::LiftedRef,
                        entry_signature: None,
                        required_evaluated: false,
                        required_generation: None,
                    }]);
                    let accepted = [AcceptedGlobal {
                        identity: binder.clone(),
                        rep: RuntimeRep::LiftedRef,
                        entry_signature: None,
                        required_evaluated: false,
                        owner: ReceiptImportOwner::Source {
                            unit: native.owner().unit.clone(),
                            module: native.owner().module.clone(),
                            module_version: Some(native.owner().module_version.clone()),
                            original_ordinal: 9001,
                            binder,
                        },
                    }];
                    let observed = AuthenticatedTargetImports::issue(
                        &prepared,
                        &accepted,
                        &validation.inventory,
                    )
                    .unwrap();
                    let demanded = inputs
                        .artifacts()
                        .inventory()
                        .select_native_target(
                            physical.physical_cohort(),
                            observed,
                            None,
                            None,
                            &[],
                            &BTreeMap::new(),
                            &mut validation,
                        )
                        .expect("restored private Full survives certification and actual demand");
                    assert!(demanded.artifacts().selected_native_groups().is_empty());
                    let groups = demanded.resolve().unwrap().groups().clone();
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
    let artifacts = right.retain_custody(&left).unwrap();
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
        source_scope: Default::default(),
        template_imports: None,
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

struct CompiledPublicationFixture {
    _fixture: crate::artifacts::test_support::SourcePublicationEntryFixture,
    producer: Vec<u8>,
    source: Arc<ExactDeclarationContext>,
    execution: Arc<OriginalExecutionEvidence>,
    original: Arc<ArtifactEntry>,
    own_ordinals: Vec<u32>,
    first: u32,
    second: u32,
}

fn compiled_publication_fixture() -> CompiledPublicationFixture {
    let fixture = crate::artifacts::test_support::prepare_source_publication_entry_fixture()
        .expect("the real completed entry retains source and native admission");
    let execution = fixture
        .entry
        .products()
        .original_execution_evidence()
        .unwrap();
    execution
        .completed()
        .expect("the actual fixture output retains its complete instance census");
    let source = execution.context().clone();
    assert_eq!(
        source.toolchain_identity_sha256(),
        CanonicalProducerIdentity::from_producer_bytes(&fixture.producer_bytes).sha256()
    );
    assert!(source.source_scope().admits(&fixture.root.descriptor.owner));
    assert!(source.lexical_graph().iter().any(|node| {
        &node.owner == execution.target() && node.imports.contains(&fixture.root.descriptor.owner)
    }));
    let own_ordinals = fixture.root.native_group_ordinals.iter().copied().collect();
    let [first, second] = fixture.root_ordinals;
    CompiledPublicationFixture {
        producer: fixture.producer_bytes.clone(),
        original: fixture.root.clone(),
        _fixture: fixture,
        source,
        execution,
        own_ordinals,
        first,
        second,
    }
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_source_proof_pairing_test owns it"]
fn complete_snapshot_recovery_keeps_proof_only_native_offer_scopes() {
    use crate::artifact_inventory::{ArtifactSelectionId, NativeArtifactDemand, ScopeProofV3};
    use crate::exact_context::{hydrate_context, ImmutableScopeProofRef, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};

    let persist = |snapshot: &crate::exact_context::ExactContextSnapshot| {
        let store = MemoryBlockstore::new();
        let mut writer = Materializer::new(&store);
        let receipt = writer.materialize_context(None, snapshot).unwrap();
        for (cid, bytes) in writer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        hydrate_context(&store, receipt.root()).unwrap()
    };
    let fixture = compiled_publication_fixture();
    let publication = fixture
        ._fixture
        .entry
        .products()
        .published_source_original_selection("revision", &fixture.original.descriptor.owner)
        .unwrap();
    let mut histories = 0;
    for (ordinals, published) in [fixture.own_ordinals.clone(), vec![fixture.second], vec![]]
        .into_iter()
        .flat_map(|ordinals| [false, true].map(move |published| (ordinals.clone(), published)))
    {
        // The compiled fixture shares immutable bytes; no live context owner
        // from this case's independent issuance backs the cold restoration.
        let (snapshot, mut acquired_inventory, id, recorded_full, carriers, retired_issuer) = {
            let producer = CanonicalProducerIdentity::from_producer_bytes(&fixture.producer);
            let entry = fixture.original.clone();
            let id = entry.descriptor.id;
            let source = if published {
                publication.context()
            } else {
                &fixture.source
            };
            let inventory = source.artifact_view().inventory();
            let initial = source
                .artifact_view()
                .select_roots(
                    source
                        .compiler_input_projection()
                        .roles()
                        .iter()
                        .map(CompilerInputRole::interface)
                        .collect(),
                )
                .unwrap();
            assert!(initial.selected_native_groups().is_empty());
            assert!(initial.capture_graph_selection().native_groups.is_empty());
            let full = inventory
                .admit_recovery_selection(
                    &initial,
                    vec![],
                    &fixture
                        .own_ordinals
                        .iter()
                        .map(|ordinal| NativeGroupKey {
                            artifact: id,
                            original_ordinal: *ordinal,
                        })
                        .collect(),
                )
                .unwrap();
            let projection = source.compiler_input_projection().clone();
            let full_support = fixture_compiler_support(projection.clone(), full.clone());
            let full_offer = full_support
                .native_offers()
                .iter()
                .find(|offer| offer.original() == id && offer.is_full())
                .unwrap();
            let carriers = full_offer.issued_group_carriers().unwrap();
            assert!(
                carriers
                    .iter()
                    .any(|(group, carrier)| group.binding.selection() != carrier.selection),
                "the fixture must issue a native group separately from its carrier"
            );
            let chosen = inventory
                .admit_recovery_selection(
                    &initial,
                    vec![],
                    &ordinals
                        .iter()
                        .map(|ordinal| NativeGroupKey {
                            artifact: id,
                            original_ordinal: *ordinal,
                        })
                        .collect(),
                )
                .unwrap();
            let available = fixture_compiler_support(projection.clone(), chosen);
            let initial_graph = initial.capture_graph_selection();
            assert_eq!(
                available.artifacts().capture_graph_selection().namespace,
                initial_graph.namespace
            );
            assert_eq!(
                full_support.artifacts().capture_graph_selection().namespace,
                initial_graph.namespace
            );
            // Only the checked availability owner transfers permission. Raw Full
            // custody independently preserves proof scopes without selecting groups.
            let baseline = fixture_compiler_support(projection, initial);
            let support = baseline
                .retain_offers(
                    full_support
                        .native_offers()
                        .iter()
                        .chain(available.native_offers().iter())
                        .cloned(),
                )
                .unwrap()
                .retain_normalized_availability(available.native_availability())
                .unwrap();
            assert_eq!(
                support.recorded_input_decisions(),
                baseline.recorded_input_decisions()
            );
            source
                .source_scope()
                .preserve_support(source.compiler_source_support(), &support)
                .unwrap();
            let context = source
                .as_ref()
                .clone()
                .rebuild(|facts| facts.compiler_support = support)
                .finish()
                .unwrap();
            let selection =
                crate::artifact_inventory::ExactArtifactSelection::capture(context.artifact_view());
            let reopened = context
                .artifact_view()
                .select_issued(vec![id], &selection)
                .unwrap();
            assert!(Arc::ptr_eq(
                reopened.selection_snapshot(),
                selection.snapshot()
            ));
            assert_eq!(
                reopened.capture_graph_selection(),
                context.artifact_view().capture_graph_selection()
            );
            let unrelated = Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    producer.sha256(),
                    "fixture",
                    "UnrequestedOfferScope",
                    BTreeMap::new(),
                ),
            ));
            let broader = context
                .artifact_view()
                .inventory()
                .admit_shared_with_demand(
                    context.artifact_view(),
                    vec![unrelated.clone()],
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            assert!(broader.selection_snapshot().scopes.len() > selection.snapshot().scopes.len());
            let narrowed = broader.select_issued(vec![id], &selection).unwrap();
            assert!(Arc::ptr_eq(
                narrowed.selection_snapshot(),
                selection.snapshot()
            ));
            assert!(!narrowed.artifact_ids().contains(&unrelated.descriptor.id));
            let mut custody_only = selection.snapshot().as_ref().clone();
            for root in custody_only.roots.keys() {
                custody_only.custody_roots.insert(*root, ());
            }
            custody_only.roots.clear();
            custody_only.namespace.clear();
            let custody_only = crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                Arc::new(custody_only),
            );
            let reopened_custody = broader.select_issued(vec![], &custody_only).unwrap();
            assert!(!reopened_custody
                .selection_snapshot()
                .custody_roots
                .is_empty());
            assert!(reopened_custody.selection_snapshot().namespace.is_empty());
            assert!(reopened_custody.selection_snapshot().roots.is_empty());
            assert!(Arc::ptr_eq(
                reopened_custody.selection_snapshot(),
                custody_only.snapshot()
            ));
            assert!(
                matches!(
                    baseline.artifacts().select_issued(vec![id], &selection),
                    Err(CompileError::ExtractFailed(detail))
                        if detail == "artifact inventory: issued selection scope is outside acquisition custody"
                ),
                "available equal payloads cannot replace an unacquired proof scope"
            );
            let proof_scope = carriers
                .keys()
                .map(|group| group.binding.selection())
                .find(|scope| {
                    !baseline
                        .artifacts()
                        .selection_snapshot()
                        .scopes
                        .contains_key(scope)
                })
                .unwrap();
            let mut altered = selection.snapshot().as_ref().clone();
            let mut proof = altered.scopes[&proof_scope].proof().as_ref().clone();
            proof.nodes.push(proof.nodes[0]);
            altered.scopes.insert(
                proof_scope,
                ImmutableScopeProofRef::from_issued(Arc::new(proof)),
            );
            let altered =
                crate::artifact_inventory::ExactArtifactSelection::from_snapshot(Arc::new(altered));
            assert!(
                matches!(
                    context.artifact_view().select_issued(vec![id], &altered),
                    Err(CompileError::ExtractFailed(detail))
                        if detail == "artifact inventory: issued selection scope proof differs from acquisition custody"
                ),
                "scope identity alone cannot authenticate different immutable proof facts"
            );
            assert_eq!(
                context
                    .compiler_input_roles()
                    .iter()
                    .any(CompilerInputRole::is_published_source_original),
                published
            );
            assert!(context.artifact_view().selected_native_groups().is_empty());
            let row = context
                .compiler_source_support()
                .native_availability()
                .rows()
                .find(|row| row.original.descriptor.id == id)
                .unwrap();
            assert_eq!(
                row.mode == NativeTransportMode::Full,
                ordinals == fixture.own_ordinals
            );
            assert_eq!(
                row.permits(fixture.first),
                ordinals.contains(&fixture.first)
            );
            assert_eq!(
                row.permits(fixture.second),
                ordinals.contains(&fixture.second)
            );
            let retired_issuer = Arc::downgrade(&context.owner);
            let snapshot = context.snapshot();
            let graph = snapshot.graph_selection().unwrap();
            let selected_scopes = graph
                .selections
                .iter()
                .map(|proof| proof.selection)
                .collect::<BTreeSet<_>>();
            assert!(
                carriers.keys().any(|group| snapshot
                    .selection()
                    .scopes
                    .contains_key(&group.binding.selection())
                    && !selected_scopes.contains(&group.binding.selection())),
                "carrier-only acquisition must omit an offer's distinct group scope"
            );
            let scratch = tempfile::tempdir().unwrap();
            let references = context
                .materialize_recovery_artifacts(
                    scratch.path(),
                    &context.artifact_view().artifact_ids().into_iter().collect(),
                )
                .unwrap();
            let recovered = RecoveredArtifactAcquisition::new(scratch.path())
                .capture_payloads_bound(
                    &references,
                    &context.artifact_view().descriptors(),
                    &graph.bindings,
                    &context.artifact_view().binding_dependencies(),
                )
                .unwrap();
            (
                snapshot,
                recovered,
                id,
                full_offer.snapshot_facts(),
                carriers,
                retired_issuer,
            )
        };
        assert!(retired_issuer.upgrade().is_none());
        let receipt = persist(&snapshot);
        let restored = acquired_inventory
            .context_for_expected_snapshot(receipt.root(), &receipt)
            .unwrap();
        assert!(restored.artifact_view().selected_native_groups().is_empty());
        assert_eq!(restored.snapshot().roles, snapshot.roles);
        assert_eq!(
            restored
                .compiler_input_roles()
                .iter()
                .any(CompilerInputRole::is_published_source_original),
            published,
        );
        assert_eq!(
            restored.snapshot().selection().as_ref(),
            snapshot.selection().as_ref()
        );
        assert_eq!(
            restored.snapshot().native_availability,
            snapshot.native_availability
        );
        assert_eq!(
            restored.snapshot().input_decisions,
            snapshot.input_decisions
        );
        let full = restored
            .compiler_source_support()
            .native_offers()
            .iter()
            .find(|offer| offer.original() == id && offer.is_full())
            .unwrap();
        assert_eq!(full.snapshot_facts(), recorded_full);
        assert_eq!(full.issued_group_carriers().unwrap(), carriers);
        if published {
            let roles = restored.compiler_input_roles();
            let selection = roles
                .iter()
                .find_map(|role| match role {
                    CompilerInputRole::PublishedSourceOriginal { selection, .. } => Some(selection),
                    _ => None,
                })
                .unwrap();
            for (scope, proof) in &selection.snapshot().scopes {
                let acquired = &restored.artifact_view().selection_snapshot().scopes[scope];
                assert!(Arc::ptr_eq(proof.proof(), acquired.proof()));
            }
        }
        let scratch = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(restored))
            .prepare_compilation(scratch.path(), &fixture.producer)
            .unwrap();
        let row = request.compiler_inputs().native_availability(id).unwrap();
        assert_eq!(
            row.permits(fixture.first),
            ordinals.contains(&fixture.first)
        );
        assert_eq!(
            row.permits(fixture.second),
            ordinals.contains(&fixture.second)
        );
        drop(request);

        // The writer and hydrator must accept these encoded bytes, so rejection
        // of an unused invalid scope is attributed to inventory authentication.
        let mut malformed = snapshot.clone();
        let bad_scope = ArtifactSelectionId([231; 32]);
        assert!(!malformed.selection().scopes.contains_key(&bad_scope));
        let bad_proof = ScopeProofV3 {
            selection: bad_scope,
            nodes: vec![ArtifactBindingNode::Artifact(ArtifactBinding {
                artifact: id,
                selection: bad_scope,
            })],
            dependencies: vec![],
        };
        Arc::make_mut(&mut malformed.selection).scopes.insert(
            bad_scope,
            ImmutableScopeProofRef::from_issued(Arc::new(bad_proof)),
        );
        let malformed_receipt = persist(&malformed);
        assert!(
            matches!(acquired_inventory.context_for_expected_snapshot(malformed_receipt.root(), &malformed_receipt),
            Err(CompileError::ExtractFailed(detail)) if detail.contains("binding scope proof digest differs from own complete rows"))
        );
        acquired_inventory.entries.remove(&id).unwrap();
        assert!(
            matches!(acquired_inventory.context_for_expected_snapshot(receipt.root(), &receipt),
            Err(CompileError::ExtractFailed(detail)) if detail.contains("missing bound recovery payload"))
        );
        histories += 1;
    }
    assert_eq!(histories, 6);
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
        source_scope: Default::default(),
        template_imports: None,
    });
    let acquire = |context: &ExactDeclarationContext, path: &Path| {
        std::fs::create_dir_all(path).unwrap();
        let (products, interfaces) = partition_native_recovery_references(
            context
                .materialize_recovery_artifacts(
                    path,
                    &context.artifact_view().artifact_ids().into_iter().collect(),
                )
                .unwrap(),
        );
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
        let support = fixture_support_with_issued_offers(
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
            source_scope: Default::default(),
            template_imports: None,
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
        let availability =
            &request.compiler_inputs().native_selection.availability.0[&entry.descriptor.id];
        assert_eq!(
            &availability.mode,
            if index == 0 {
                &NativeTransportMode::Full
            } else {
                &NativeTransportMode::IssuedGroups
            }
        );
        if index != 0 {
            assert!(availability.issued_ordinals().is_empty());
        }
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
                        let support = fixture_support_with_issued_offers(
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
                                source_scope: Default::default(),
                                template_imports: None,
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
                        let (products, interfaces) = partition_native_recovery_references(
                            context
                                .materialize_recovery_artifacts(
                                    &path,
                                    &context.artifact_view().artifact_ids().into_iter().collect(),
                                )
                                .unwrap(),
                        );
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
                        let availability =
                            &request.compiler_inputs().native_selection.availability.0;
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
                source_scope: Default::default(),
                template_imports: None,
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
            source_scope: Default::default(),
            template_imports: None,
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
    let private = OriginalCompilerInputs::from_issued_support(inputs.compiler_source_support());
    assert!(private
        .artifacts
        .artifact_ids()
        .contains(&old_entry.descriptor.id));
    let clean = issue(baseline.clone(), "clean");
    let clean = OriginalCompilerInputs::from_issued_support(
        clean.compiler_inputs().compiler_source_support(),
    );
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
#[ignore = "requires the declared rooted compiler service; toolchain_source_proof_pairing_test owns it"]
fn retained_template_capture_uses_selected_canonical_with_historical_custody() {
    use crate::artifact_inventory::IssuedNativeOffer;
    let fixture = compiled_publication_fixture();
    let actual_owner = fixture.original.descriptor.owner.clone();
    let captured = CapturedTemplateImports::capture(
        &fixture.execution,
        &[format!("import {}\n", actual_owner.module)],
    )
    .unwrap()
    .expect("the actual output admits its selected template source");
    let actual_canonical =
        fixture.source.compiler_input_projection().snapshot_roles()[&actual_owner].interface();
    assert_eq!(
        captured.evidence.graph[&actual_owner].canonical,
        actual_canonical
    );
    captured
        .evidence
        .validate(
            fixture.source.toolchain_identity_sha256(),
            fixture.source.artifact_view(),
        )
        .unwrap();
    captured
        .evidence
        .validate(
            fixture.source.toolchain_identity_sha256(),
            captured.compiler_support.artifacts(),
        )
        .expect("the actual template evidence validates against its narrowed TYPE custody");
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
    let mut wrong_seal = captured.evidence.clone();
    wrong_seal
        .graph
        .get_mut(&actual_owner)
        .unwrap()
        .interface
        .interface_sha256[0] ^= 1;
    assert!(wrong_seal
        .validate(
            fixture.source.toolchain_identity_sha256(),
            fixture.source.artifact_view()
        )
        .is_err());
    let (actual_support, _) = captured
        .into_value_support(fixture.source.compiler_source_support().clone())
        .unwrap();
    assert_eq!(
        actual_support
            .artifacts()
            .capture_graph_selection()
            .namespace,
        fixture
            .source
            .artifact_view()
            .capture_graph_selection()
            .namespace
    );
    assert_eq!(
        actual_support.artifacts().capture_graph_selection().roots,
        fixture
            .source
            .artifact_view()
            .capture_graph_selection()
            .roots
    );

    // Retained TYPE and native-availability histories do not issue another
    // output or a completed template capture.
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
            let original = ExactDeclarationContext::retained_execution_support_fixture(
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
            let retained = original
                .compiler_source_support()
                .selected_interfaces()
                .unwrap()
                .support_for_owners(vec![owner.clone()])
                .unwrap()
                .retain_template_availability(original.compiler_source_support())
                .unwrap();
            assert_eq!(
                retained.projection().snapshot_roles()[&owner].interface(),
                expected
            );
            assert_eq!(
                retained.artifacts().capture_graph_selection(),
                expected_graph
            );
            assert_eq!(
                retained.projection().roles(),
                vec![CompilerInputRole::InterfaceOnly {
                    interface: expected
                }]
            );
            assert_eq!(original.lexical_graph(), before_lexical);
            assert!(retained.artifacts().selected_native_groups().is_empty());
            assert_eq!(original.artifact_view().capture_graph_selection(), before);
            let retained_support = original.compiler_source_support().merge(&retained).unwrap();
            let retained_again = retained_support.artifacts();
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
        contextual_native_availability: vec![fresh],
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
        assert_eq!(inputs.projection, *context.compiler_input_projection());
        assert_eq!(
            inputs.native_availability(),
            context.compiler_source_support().native_availability()
        );
        assert_eq!(
            inputs.issued_offers(),
            context.compiler_source_support().native_offers()
        );
        assert!(inputs
            .issued_artifacts()
            .artifact_ids()
            .contains(&fresh_entry.descriptor.id));
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context.clone()))
            .prepare_compilation_with_private_input(directory.path(), PRODUCER, None, Some(inputs))
            .unwrap();
        assert!(request
            .compiler_inputs()
            .native_availability(fresh_entry.descriptor.id)
            .is_none());
        histories += 1;
    }
    assert_eq!(histories, 5);
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_source_proof_pairing_test owns it"]
fn candidate_reuse_keeps_full_availability_separate_from_withdrawn_demand() {
    let fixture = compiled_publication_fixture();
    let producer = CanonicalProducerIdentity::from_producer_bytes(&fixture.producer);
    let entry = fixture.original.clone();
    let ArtifactPayload::Original(native) = &entry.payload else {
        unreachable!()
    };
    let publication = fixture
        ._fixture
        .entry
        .products()
        .published_source_original_selection("revision", &entry.descriptor.owner)
        .unwrap();
    let published = publication.context();
    let role =
        published.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner].clone();
    let empty = published
        .compiler_source_support()
        .select_support_roots(
            published
                .compiler_input_projection()
                .roles()
                .into_iter()
                .map(|role| role.interface())
                .collect(),
        )
        .unwrap();
    let fresh = recovered_witness_fixtures(&[fixture_finalized_product(
        original_groups_fixture_with_interface(
            "FreshBesidePublishedZeroDemand",
            vec![(7, vec![]), (9001, vec![])],
            138,
            &BTreeMap::new(),
            b"fresh unrelated native interface".to_vec(),
        ),
        producer.sha256(),
    )])
    .remove(0)
    .product;
    for full_demand in [true, false] {
        let support = if full_demand {
            published.compiler_source_support().clone()
        } else {
            empty.clone()
        };
        assert!(support
            .native_offers()
            .iter()
            .any(|offer| offer.original() == entry.descriptor.id && offer.is_full()));
        let canonical = ArtifactEntry::canonical_artifact_id(native.module_interface().unwrap());
        assert!(matches!(
            support.checked_native_input_decision(entry.descriptor.id, canonical, native.owner()).unwrap(),
            crate::artifact_inventory::InputNativeDecision::IssuedChoices(choices)
                if choices.is_empty() != full_demand
        ));
        let context = published
            .as_ref()
            .clone()
            .rebuild(|facts| facts.compiler_support = support)
            .finish()
            .unwrap();
        assert_eq!(
            context.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner],
            role
        );
        for mixed_fresh in [false, true] {
            let candidates = crate::module_candidates::CandidateSet {
                manifest_path: PathBuf::from("candidate-native-issuance-only"),
                by_owner: BTreeMap::new(),
                contextual_native_availability: if mixed_fresh {
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
            let reused_role = reused
                .projection
                .snapshot_roles()
                .get(&entry.descriptor.owner)
                .unwrap();
            assert_eq!(reused_role, &role);
            assert!(matches!(
                reused.issued_input_decisions().unwrap().for_role(&entry.descriptor.owner, reused_role),
                crate::artifact_inventory::InputNativeDecision::IssuedChoices(choices)
                    if choices.is_empty() != full_demand
            ));
            let directory = tempfile::tempdir().unwrap();
            let request = ExactCompileContext::new(Arc::new(context.clone()))
                .prepare_compilation_with_private_input(
                    directory.path(),
                    &fixture.producer,
                    None,
                    Some(reused),
                )
                .unwrap();
            let inputs = request.compiler_inputs();
            assert_eq!(
                inputs.projection().snapshot_roles()[&entry.descriptor.owner],
                role
            );
            assert!(matches!(
                inputs.compiler_source_support()
                    .checked_native_input_decision(entry.descriptor.id, canonical, native.owner())
                    .unwrap(),
                crate::artifact_inventory::InputNativeDecision::IssuedChoices(choices)
                    if choices.is_empty() != full_demand
            ));
            let mut validation = PackageInterfaceValidation::default();
            let mut selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                    inputs,
                    &validation.inventory,
                )
                .unwrap();
            // Full availability supplies exact body/dependency support in both
            // requests. Executable demand remains the separate carried choice.
            for ordinal in &fixture.own_ordinals {
                assert!(selection.permits_native_group(
                    entry.descriptor.id,
                    native.owner(),
                    *ordinal,
                ));
            }
            let physical = CapturedCompilerStage::issue(
                &mut selection,
                inputs.artifacts(),
                Vec::new(),
                &mut validation,
            )
            .unwrap();
            let definitions =
                crate::certified_products::original_native_group_definitions(native, fixture.first)
                    .unwrap();
            let contract = definitions
                .export_contracts()
                .map(Result::unwrap)
                .find(|contract| contract.identity.occurrence == "first")
                .unwrap();
            let binder = contract.identity.clone();
            let prepared = prepared_target_fixture(vec![GlobalDecl {
                identity: binder.clone(),
                rep: contract.rep,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            }]);
            let accepted = [AcceptedGlobal {
                identity: binder.clone(),
                rep: contract.rep,
                entry_signature: None,
                required_evaluated: false,
                owner: ReceiptImportOwner::Source {
                    unit: native.owner().unit.clone(),
                    module: native.owner().module.clone(),
                    module_version: Some(native.owner().module_version.clone()),
                    original_ordinal: fixture.first,
                    binder,
                },
            }];
            let observed =
                AuthenticatedTargetImports::issue(&prepared, &accepted, &validation.inventory)
                    .unwrap();
            let admitted = inputs
                .artifacts()
                .inventory()
                .select_native_target(
                    physical.physical_cohort(),
                    observed,
                    None,
                    None,
                    &[],
                    &BTreeMap::new(),
                    &mut validation,
                )
                .map(|_| ());
            if full_demand {
                assert!(
                    admitted.is_ok(),
                    "issued Full demand must reach native admission: {admitted:?}"
                );
            } else {
                assert!(
                    matches!(&admitted, Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal }
                        if artifact == entry.descriptor.id && original_ordinal == fixture.first)
                    && matches!(&error.native_refusal,
                        Some(crate::artifact_inventory::NativeAdmissionRefusal::RootChoiceExplicitEmpty {
                            owner,
                            original_ordinal,
                        }) if owner == &entry.descriptor.owner && *original_ordinal == fixture.first)),
                    "Full availability cannot resurrect explicit Empty demand: {admitted:?}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_source_proof_pairing_test owns it"]
fn authored_fallback_does_not_requalify_unrelated_selected_published_owner() {
    let directory = crate::artifacts::test_support::compiler_input_directory().unwrap();
    let sources = directory.path().join("sources");
    std::fs::create_dir(&sources).unwrap();
    let input = directory.path().join("quote-input");
    let executions = directory.path().join("quote-input.executions");
    std::fs::write(
        sources.join("QuotedProvider.hs"),
        include_str!("../../tests/fixtures/completed-source/QuotedProvider.hs"),
    )
    .unwrap();
    std::fs::write(
        sources.join("QuotedOriginal.hs"),
        include_str!("../../tests/fixtures/completed-source/QuotedOriginal.hs")
            .replace("QUOTE_INPUT_PATH", input.to_str().unwrap()),
    )
    .unwrap();
    let target_path = sources.join("PreparedOriginal.hs");
    std::fs::write(
        &target_path,
        include_str!("../../tests/fixtures/completed-source/PreparedOriginal.hs"),
    )
    .unwrap();
    let frozen = crate::artifacts::FrozenEntrySources::capture(&[sources], &target_path).unwrap();
    let endpoint = tidepool_extract_cmd::ExtractCmd::new()
        .unwrap()
        .bind()
        .unwrap();
    let producer =
        CanonicalProducerIdentity::from_producer_bytes(endpoint.identity().producer_bytes());
    let prepare = |name: &str| {
        crate::artifacts::test_support::prepare_frozen_production_entry(
            &frozen,
            directory.path(),
            &directory.path().join(name),
        )
        .unwrap()
    };
    std::fs::write(&input, "37").unwrap();
    let historical_output = prepare("historical");
    let previous_executions = std::fs::read_to_string(&executions).unwrap();
    assert!(!previous_executions.is_empty());
    assert!(previous_executions.lines().all(|value| value == "37"));
    std::fs::write(&input, "91").unwrap();
    let current_output = prepare("current");
    let current_executions = std::fs::read_to_string(&executions).unwrap();
    let new_executions = current_executions
        .strip_prefix(&previous_executions)
        .unwrap();
    assert!(
        !new_executions.is_empty(),
        "the second producer executes its quotation"
    );
    assert!(new_executions.lines().all(|value| value == "91"));
    let historical_source = historical_output
        .products()
        .original_execution_context()
        .unwrap();
    let current_source = current_output
        .products()
        .original_execution_context()
        .unwrap();
    assert_eq!(
        historical_source.toolchain_identity_sha256(),
        producer.sha256()
    );
    assert_eq!(
        current_source.toolchain_identity_sha256(),
        producer.sha256()
    );
    let original = |context: &ExactDeclarationContext| {
        context
            .artifact_view()
            .entries()
            .into_iter()
            .find(|entry| {
                entry.descriptor.owner.module == "QuotedOriginal"
                    && matches!(entry.payload, ArtifactPayload::Original(_))
            })
            .expect("the actual producer captured QuotedOriginal")
    };
    let old = original(&historical_source);
    let entry = original(&current_source);
    let (ArtifactPayload::Original(historical), ArtifactPayload::Original(current)) =
        (&old.payload, &entry.payload)
    else {
        unreachable!()
    };
    assert!(matches!(
        current.module_interface().unwrap().origin(),
        crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
    ));
    assert_eq!(old.descriptor.owner, entry.descriptor.owner);
    // A changed quotation may change canonical Core and interface evidence.
    // Historical custody must not replace the exact current Source selection.
    assert_ne!(
        entry.descriptor.id, old.descriptor.id,
        "the external quotation input must produce distinct native bodies"
    );
    assert!(current_source
        .source_scope()
        .admits(&entry.descriptor.owner));
    let old_offers = historical_source
        .compiler_source_support()
        .native_offers()
        .iter()
        .filter(|offer| offer.original() == old.descriptor.id)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        old_offers.iter().any(|offer| offer.is_full()),
        "the historical producer really issued a Full offer for this body"
    );

    // A remains a valid exact historical Source selection in its own request.
    // The refusal below is scoped to the current request selecting B.
    let historical_request = ExactCompileContext::new(historical_source.clone())
        .prepare_compilation(
            &directory.path().join("historical-request"),
            endpoint.identity().producer_bytes(),
        )
        .unwrap();
    let historical_canonical =
        ArtifactEntry::canonical_artifact_id(historical.module_interface().unwrap());
    assert!(matches!(
        historical_request
            .compiler_inputs()
            .compiler_source_support()
            .checked_native_input_decision(
                old.descriptor.id,
                historical_canonical,
                historical.owner()
            )
            .unwrap(),
        crate::artifact_inventory::InputNativeDecision::IssuedChoices(_)
    ));

    let module = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(2)).module_name();
    let fallback = crate::certified_products::fixture_native_declaration_product(
        crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
            "main",
            &module,
            vec![(7, vec![]), (9001, vec![])],
            153,
            &BTreeMap::new(),
            b"unrelated authored canonical interface".to_vec(),
            &BTreeMap::new(),
            &[],
        ),
        producer.sha256(),
        2,
    );
    let fallback = recovered_witness_fixtures(&[fallback]).remove(0).product;
    let target = Arc::new(ArtifactEntry::canonical(
        fallback.module_interface().unwrap().clone(),
    ));
    let inventory = ArtifactInventory::default();
    let interfaces = inventory
        .admit_shared(&inventory.empty_view(), vec![target.clone()])
        .unwrap();
    let fallback_context = ExactDeclarationContext::from_authenticated_interfaces(
        producer.sha256(),
        &interfaces,
        CompilerInputProjection::from_issued_entries(std::slice::from_ref(&target)).unwrap(),
    )
    .unwrap();
    assert!(fallback_context.source_scope().rows().is_empty());
    let source = current_source
        .as_ref()
        .clone()
        .extend_interface_context(&fallback_context)
        .unwrap();
    let publication = current_output
        .products()
        .published_source_original_selection("revision", &entry.descriptor.owner)
        .unwrap();
    let published_with_fallback = publication
        .context()
        .as_ref()
        .clone()
        .extend_interface_context(&fallback_context)
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
            .retain_issued_native_offers(old_offers.iter().cloned())
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
        let request = ExactCompileContext::new(Arc::new(mixed.clone()))
            .prepare_compilation(
                &directory
                    .path()
                    .join(format!("current-request-{published}")),
                endpoint.identity().producer_bytes(),
            )
            .unwrap();
        let support = request.compiler_inputs().compiler_source_support();
        let current_canonical =
            ArtifactEntry::canonical_artifact_id(current.module_interface().unwrap());
        assert!(matches!(
            support
                .checked_native_input_decision(
                    entry.descriptor.id,
                    current_canonical,
                    current.owner()
                )
                .unwrap(),
            crate::artifact_inventory::InputNativeDecision::IssuedChoices(_)
        ));
        let crossed = support
            .checked_native_input_decision(
                old.descriptor.id,
                historical_canonical,
                historical.owner(),
            )
            .map(|_| ());
        assert!(
            matches!(crossed,
            Err(crate::certified_products::InputDecisionRefusal::ArtifactIdentityMismatch(ref mismatch))
                if mismatch.stage == crate::certified_products::ArtifactIdentityStage::RequestedIdentityDiffers
                    && mismatch.expected_original == Some(old.descriptor.id)
                    && mismatch.observed_original == Some(entry.descriptor.id)
                    && mismatch.expected_canonical == Some(historical_canonical)
                    && mismatch.observed_canonical == Some(current_canonical)),
            "historical A cannot replace the current Source slot selecting B: {crossed:?}"
        );
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
fn candidate_custody_preserves_issued_availability_without_selecting_execution() {
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
    let output = (|| -> Result<OriginalCompilerInputs, CompileError> {
        let fixture_products: &[CertifiedRecoveryProduct] = std::slice::from_ref(&dependency);
        let entries = fixture_products
            .iter()
            .map(|product| {
                ArtifactEntry::original(producer.sha256(), product.clone()).map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let inventory = ArtifactInventory::default();
        let artifacts = inventory.admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )?;
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
        CapturedCompilerStage::issue(&mut selection, &artifacts, Vec::new(), &mut validation)
            .map(|stage| stage.compiler_inputs().as_ref().clone())
    })()
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
    // Available groups live in retained scope proofs, independently of execution selection.
    let retained_groups = issued
        .selection_snapshot()
        .scopes
        .values()
        .flat_map(|scope| scope.proof().nodes.iter())
        .filter_map(|node| match node {
            ArtifactBindingNode::Group(group)
                if group.binding.artifact() == entry.descriptor.id =>
            {
                Some(*group)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        retained_groups
            .iter()
            .map(|group| (group.binding.artifact(), group.original_ordinal))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(entry.descriptor.id, 3), (entry.descriptor.id, 7)]),
        "retained proof membership must match the independently authored groups",
    );
    assert_eq!(
        full.issued_group_carriers()
            .unwrap()
            .into_keys()
            .collect::<BTreeSet<_>>(),
        retained_groups,
        "checked offer membership must match the exact retained issued groups",
    );
    assert!(support.artifacts().selected_native_groups().is_empty());
    let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer: producer.sha256(),
        compiler_support: support,
        lexical: Default::default(),
        source_scope: Default::default(),
        template_imports: None,
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
        contextual_native_availability: vec![fresh],
        catalog_owner: None,
        input_transport: Vec::new(),
    };
    let assert_candidate_custody = |context: &ExactDeclarationContext| {
        let before = context.artifact_view().capture_graph_selection();
        let inputs = OriginalCompilerInputs::from_candidates(context, producer, &candidates)
            .expect("candidate bytes do not demand fresh native permission")
            .expect("candidate bytes produce a custody fragment");
        assert!(inputs.issued_artifacts().artifact_ids().contains(&fresh_id));
        assert!(inputs
            .issued_artifacts()
            .selected_native_groups()
            .is_empty());
        assert_eq!(
            inputs.role_projection().snapshot_roles(),
            context.compiler_input_projection().snapshot_roles(),
            "candidate custody cannot select a fresh compiler role"
        );
        assert_eq!(
            inputs.native_availability().snapshot_facts().unwrap(),
            context
                .compiler_source_support()
                .native_availability()
                .snapshot_facts()
                .unwrap(),
            "all existing permission rows survive unchanged"
        );
        assert_eq!(
            inputs.native_offers().len(),
            context.compiler_source_support().native_offers().len()
        );
        for (retained, issued) in inputs
            .native_offers()
            .iter()
            .zip(context.compiler_source_support().native_offers())
        {
            assert!(retained.same_issuance(issued));
            assert_eq!(retained.snapshot_facts(), issued.snapshot_facts());
        }
        assert!(!inputs
            .native_offers()
            .iter()
            .any(|offer| offer.original() == fresh_id));
        assert!(!inputs
            .native_availability()
            .snapshot_facts()
            .unwrap()
            .contains_key(&fresh_id));
        assert_eq!(context.artifact_view().capture_graph_selection(), before);
    };
    assert_candidate_custody(&context);

    // A continued input carries its issued row permissions. It has no new
    // product receipt and cannot authorize a fresh body alongside those rows.
    let continue_request = |context: &ExactDeclarationContext| -> Result<_, CompileError> {
        let request = ProtectedScaffoldRequirements::default().compiler_inputs(context, None)?;
        let validation = PackageInterfaceValidation::default();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            &request,
            &validation.inventory,
        )
        .map_err(compiler_evidence_failure)?;
        assert!(selection
            .complete_native_entries(request.artifacts(), &validation.inventory)
            .map_err(compiler_evidence_failure)?
            .is_empty());
        let continued =
            OriginalCompilerInputs::from_issued_support(request.compiler_source_support());
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
        assert_candidate_custody(&bounded_context);
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
        assert_candidate_custody(&scoped);
    }
    let historical_support =
        fixture_compiler_support(context.compiler_input_projection().clone(), historical_view);
    let historical_context = context.clone().rebuild(|facts| {
        facts.compiler_support = historical_support;
    });
    let mismatched_issuer = context.compiler_source_support().restore_snapshot_evidence(
        &historical_context.snapshot(),
        &BTreeMap::new(),
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    );
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

pub(super) fn native_context(product: &CertifiedRecoveryProduct) -> Arc<ExactDeclarationContext> {
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
            source_scope: Default::default(),
            template_imports: None,
        },
    ))
}

#[test]
fn source_support_with_empty_native_permissions_discards_unrelated_parent_custody() {
    let inventory = ArtifactInventory::default();
    let entries = ["NarrowedRoot", "UnrelatedSibling"].map(|module| {
        Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "fixture",
                module,
                BTreeMap::new(),
            ),
        ))
    });
    let parent = inventory
        .admit_shared(&inventory.empty_view(), entries.to_vec())
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let support = recover_fixture_compiler_support(projection, parent, []).unwrap();
    assert!(support.native_availability().rows().next().is_none());
    assert_eq!(support.acquired_native_closures().iter().count(), 0);
    assert!(support
        .artifacts()
        .artifact_ids()
        .contains(&entries[1].descriptor.id));
    let narrowed = support
        .source_support(&BTreeSet::from([entries[0].descriptor.owner.clone()]))
        .unwrap();
    assert_eq!(
        narrowed.artifacts().artifact_ids(),
        vec![entries[0].descriptor.id]
    );
    assert_eq!(narrowed.projection().roles().len(), 1);
    assert!(narrowed.artifacts().selected_native_groups().is_empty());
    let empty = support.source_support(&BTreeSet::new()).unwrap();
    assert!(empty.artifacts().artifact_ids().is_empty());
    assert!(empty.projection().roles().is_empty());
    assert!(empty.artifacts().selected_native_groups().is_empty());
}

#[test]
fn source_support_retains_every_original_in_a_shared_native_contribution() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let native = |module: &str, ordinal: u32, seed: u8| {
        fixture_finalized_product(
            original_groups_fixture_with_interface(
                module,
                vec![(ordinal, vec![])],
                seed,
                &BTreeMap::new(),
                module.as_bytes().to_vec(),
            ),
            producer,
        )
    };
    let left = native("SharedContributionLeft", 7, 61);
    let right = native("SharedContributionRight", 11, 62);
    let sibling = native("SharedContributionSibling", 13, 63);
    let root = fixture_finalized_product(
        original_groups_fixture_with_interface(
            "SharedContributionRoot",
            vec![(17, vec![source_import(&left, 7), source_import(&right, 11)])],
            64,
            &BTreeMap::new(),
            b"shared contribution root".to_vec(),
        ),
        producer,
    );
    let products = recovered_witness_fixtures(&[left, right, sibling, root])
        .into_iter()
        .map(|witness| witness.product)
        .collect::<Vec<_>>();
    let entries = products
        .iter()
        .map(|product| Arc::new(ArtifactEntry::original(producer, product.clone()).unwrap()))
        .collect::<Vec<_>>();
    let ids = entries
        .iter()
        .map(|entry| entry.descriptor.id)
        .collect::<Vec<_>>();
    let [left_id, right_id, sibling_id, root_id] = ids.as_slice() else {
        unreachable!()
    };

    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared(&inventory.empty_view(), entries.clone())
        .unwrap();
    let root_view = artifacts.select_roots(vec![*root_id]).unwrap();
    let proof = crate::artifact_inventory::NativeOriginalRequirement::from_selected_view(
        *root_id, &root_view,
    )
    .unwrap()
    .unwrap();
    let offer =
        crate::artifact_inventory::IssuedNativeOffer::issue(*root_id, &root_view, Some(proof))
            .unwrap();
    let shared = Arc::new(
        offer
            .contribution_for(&BTreeSet::from([*left_id, *right_id]))
            .unwrap()
            .expect("the root offer carries both child closures"),
    );
    assert_eq!(
        shared.originals(),
        &BTreeSet::from([*left_id, *right_id]),
        "the fixture must share exactly the two disjoint row closures"
    );
    assert_eq!(shared.original_groups(*left_id).count(), 1);
    assert_eq!(shared.original_groups(*right_id).count(), 1);

    let row = |entry: &Arc<ArtifactEntry>| {
        Arc::new(NativeTransportAvailability {
            canonical: NativeTransportAvailability::canonical_for(entry).unwrap(),
            original: Arc::clone(entry),
            mode: NativeTransportMode::IssuedGroups,
            classification: NativeInputClassification::CompilerRole,
            contributions: Arc::from([Arc::clone(&shared)]),
        })
    };
    let rows =
        RequestNativeAvailability::from_issued_rows([row(&entries[0]), row(&entries[1])]).unwrap();
    assert_eq!(rows.custody_contributions().len(), 2);

    let projection = CompilerInputProjection::from_issued_entries(&[
        Arc::clone(&entries[3]),
        Arc::clone(&entries[0]),
        Arc::clone(&entries[1]),
    ])
    .unwrap()
    .interface_only();
    let support = recover_fixture_compiler_support(projection, artifacts, [offer])
        .unwrap()
        .retain_normalized_availability(&rows)
        .unwrap();
    assert!(
        support.artifacts().artifact_ids().contains(sibling_id),
        "narrowing must start with the unrelated sibling physically retained"
    );
    let narrowed = support
        .source_support(&BTreeSet::from([
            entries[0].descriptor.owner.clone(),
            entries[1].descriptor.owner.clone(),
        ]))
        .unwrap();
    assert!(
        narrowed.native_offers().is_empty(),
        "the removed root offer must not independently retain the required child custody"
    );
    let contributions = narrowed.native_availability().custody_contributions();
    assert_eq!(contributions.len(), 2);
    assert_eq!(
        contributions
            .iter()
            .map(|(id, _)| *id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([*left_id, *right_id])
    );
    assert!(
        Arc::ptr_eq(&contributions[0].1, &contributions[1].1),
        "both required Originals must still share one contribution"
    );
    let retained = narrowed.artifacts();
    let captured = retained.capture_graph_selection();
    assert_eq!(
        captured
            .native_groups
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        shared
            .original_groups(*left_id)
            .chain(shared.original_groups(*right_id))
            .collect(),
        "narrowing retains exactly the two required issued Group closures"
    );
    for (id, owner) in [
        (*left_id, &entries[0].descriptor.owner),
        (*right_id, &entries[1].descriptor.owner),
    ] {
        assert!(
            retained.artifact_ids().contains(&id),
            "both shared-contribution rows retain their exact Original"
        );
        let group = shared.original_groups(id).next().unwrap();
        assert!(
            captured.native_groups.contains(&group),
            "both shared-contribution rows retain their exact Group closure"
        );
        assert!(
            narrowed.projection().snapshot_roles()[owner]
                .original()
                .is_none(),
            "private native custody does not promote its TYPE row to an Original role"
        );
    }
    assert!(
        !retained.artifact_ids().contains(sibling_id),
        "the root's whole inventory does not retain an unrelated sibling body"
    );
    assert!(
        captured
            .native_groups
            .iter()
            .all(|group| group.binding.artifact() != *sibling_id),
        "the unrelated sibling remains non-executable"
    );
    assert!(
        !narrowed
            .projection()
            .snapshot_roles()
            .contains_key(&entries[2].descriptor.owner),
        "the unrelated sibling also gains no compiler role"
    );

    // The actual request consumer must retain both exact dependency bindings
    // without restoring the parent's unrelated acquired payload.
    let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer,
        compiler_support: narrowed,
        lexical: Default::default(),
        source_scope: Default::default(),
        template_imports: None,
    });
    let directory = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(Arc::new(context))
        .prepare_compilation(directory.path(), PRODUCER)
        .unwrap();
    let permissions = request.compiler_inputs().private_native_availability();
    for (id, ordinal, product) in [(*left_id, 7, &products[0]), (*right_id, 11, &products[1])] {
        assert!(permissions
            .dependency_support(
                NativeGroupKey {
                    artifact: id,
                    original_ordinal: ordinal,
                },
                product.owner()
            )
            .is_some());
        assert!(
            permissions
                .dependency_support(
                    NativeGroupKey {
                        artifact: id,
                        original_ordinal: ordinal + 1,
                    },
                    product.owner()
                )
                .is_none(),
            "custody does not authorize an unissued ordinal"
        );
        assert!(
            permissions
                .dependency_support(
                    NativeGroupKey {
                        artifact: id,
                        original_ordinal: ordinal,
                    },
                    products[2].owner()
                )
                .is_none(),
            "custody does not authorize another native owner"
        );
    }
    assert!(permissions
        .dependency_support(
            NativeGroupKey {
                artifact: *sibling_id,
                original_ordinal: 13,
            },
            products[2].owner()
        )
        .is_none());
    let consumed = request.compiler_inputs().artifacts();
    assert!(!consumed.artifact_ids().contains(sibling_id));
    let graph = consumed.capture_graph_selection();
    let edges = consumed.binding_dependencies();
    let restored_inventory = ArtifactInventory::default();
    let restored = restored_inventory
        .recover_selection(
            &restored_inventory.empty_view(),
            consumed.entries(),
            &graph,
            &edges,
        )
        .unwrap();
    assert_eq!(restored.capture_graph_selection(), graph);
    assert_eq!(restored.binding_dependencies(), edges);
    for view in [consumed, &restored] {
        for id in [*left_id, *right_id] {
            let ordinal = shared.original_groups(id).next().unwrap().original_ordinal;
            view.native_requirements_from_roots(&[
                crate::artifact_inventory::NativeRequirementRoot::Group {
                    artifact: id,
                    original_ordinal: ordinal,
                },
            ])
            .expect("the native consumer resolves each required exact child after narrowing");
        }
    }

    let left_only = inventory
        .admit_shared(&inventory.empty_view(), vec![Arc::clone(&entries[0])])
        .unwrap();
    assert!(!left_only.artifact_ids().contains(right_id));
    assert!(matches!(
        rows.retain_acquisitions(&left_only),
        Err(CompileError::ArtifactInventory(error))
            if matches!(
                error.failure,
                crate::artifact_inventory::ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact
                } if artifact == *right_id
            )
    ));
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
#[ignore = "requires the declared rooted compiler service; toolchain_source_proof_pairing_test owns it"]
fn retained_template_capture_preserves_normalized_input_modes_with_broader_proof_custody() {
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    let fixture = compiled_publication_fixture();
    let entry = fixture.original.clone();
    let owner = entry.descriptor.owner.clone();
    let producer = CanonicalProducerIdentity::from_producer_bytes(&fixture.producer).sha256();
    let source = &fixture.source;
    let captured = CapturedTemplateImports::capture(
        &fixture.execution,
        &[format!("import {}\n", owner.module)],
    )
    .unwrap()
    .expect("the actual output admits its selected template source");
    captured
        .evidence
        .validate(producer, source.artifact_view())
        .unwrap();
    captured
        .evidence
        .validate(producer, captured.compiler_support.artifacts())
        .expect("the actual template evidence validates against its narrowed TYPE custody");
    let interfaces = source
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(captured.evidence.graph.keys().cloned().collect())
        .unwrap();
    let (_, evidence) = captured
        .into_value_support(crate::artifact_inventory::CompilerSourceSupport::empty())
        .unwrap();
    let inventory = source.artifact_view().inventory();
    let initial = source
        .artifact_view()
        .select_roots(
            source
                .compiler_input_projection()
                .roles()
                .iter()
                .map(CompilerInputRole::interface)
                .collect(),
        )
        .unwrap();
    let projection = source.compiler_input_projection().clone();
    let full_support = source.compiler_source_support();
    assert!(full_support
        .native_offers()
        .iter()
        .any(|offer| offer.is_full()));
    let mut previous: Option<(Arc<TemplateImportEvidence>, ExactDeclarationContext)> = None;
    for ordinals in [
        fixture.own_ordinals.clone(),
        vec![fixture.first],
        vec![fixture.second],
        vec![],
    ] {
        let selected = ordinals
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            })
            .collect();
        let chosen = inventory
            .admit_recovery_selection(&initial, vec![], &selected)
            .unwrap();
        let support = fixture_compiler_support(projection.clone(), chosen)
            .retain_offers(full_support.native_offers().iter().cloned())
            .unwrap();
        assert!(support.native_offers().iter().any(|offer| offer.is_full()));
        let retained = interfaces.retain_template_availability(&support).unwrap();
        assert!(retained
            .projection()
            .roles()
            .iter()
            .all(|role| role.original().is_none()));
        assert!(retained.artifacts().selected_native_groups().is_empty());
        let exported = ExactDeclarationContext::empty()
            .extend_issued_compiler_support(&retained)
            .unwrap()
            .rebuild(|facts| facts.template_imports = Some(evidence.clone()))
            .finish()
            .unwrap();
        if let Some((prior_evidence, prior_context)) = &previous {
            assert_eq!(
                &evidence, prior_evidence,
                "recipe evidence does not encode native permissions"
            );
            assert_ne!(
                exported, *prior_context,
                "equal template evidence cannot erase different compiler support"
            );
            assert_ne!(
                exported.snapshot().native_availability,
                prior_context.snapshot().native_availability
            );
        }
        previous = Some((evidence.clone(), exported.clone()));
        let root = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(exported.clone()))
            .prepare_compilation(root.path(), &fixture.producer)
            .unwrap();
        let row = request
            .compiler_inputs()
            .native_availability(entry.descriptor.id)
            .expect("explicit empty remains an issued row");
        assert_eq!(
            row.mode == NativeTransportMode::Full,
            ordinals == fixture.own_ordinals
        );
        assert_eq!(
            row.permits(fixture.first),
            ordinals.contains(&fixture.first)
        );
        assert_eq!(
            row.permits(fixture.second),
            ordinals.contains(&fixture.second)
        );
        assert_eq!(
            wire_mode(&request, &owner.module),
            if ordinals == fixture.own_ordinals {
                full_mode()
            } else {
                issued_mode(&ordinals)
            },
            "capture must advertise the actual mode, including explicit Empty"
        );
        let inputs = request.compiler_inputs();
        let private = inputs.private_native_availability();
        let advertised = inputs.advertised_native_originals().unwrap();
        let receipt = advertised
            .receipt_original(
                &private,
                &owner.unit,
                &owner.module,
                &row.original_owner().module_version,
            )
            .unwrap()
            .expect("captured carrier identity remains advertised without granting groups");
        assert_eq!(receipt.original.descriptor.id, entry.descriptor.id);
        assert_eq!(
            receipt.permits(fixture.first),
            ordinals.contains(&fixture.first)
        );
        assert_eq!(
            receipt.permits(fixture.second),
            ordinals.contains(&fixture.second)
        );
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
        if ordinals == fixture.own_ordinals {
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
        if ordinals != fixture.own_ordinals {
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
    Arc::make_mut(&mut Arc::make_mut(&mut metadata.retained).selected_native_groups)
        .insert(unsupported);
    let error =
        NativeInputSelection::issue_checked(&projection, &metadata, &view, support.native_offers())
            .err()
            .expect("whole selected ingress must validate all its own selected obligations");
    assert!(matches!(error, CompileError::ExtractFailed(message)
    if message == format!(
        "exact declaration context: selected native group is outside issued availability: stage=whole-selected owner={:?} group={unsupported:?}",
        entry.descriptor.owner,
    )));
}

#[test]
fn completed_request_carries_exact_entries_without_reconstruction() {
    use crate::artifact_inventory::{original_entry_construction_calls, NativeArtifactDemand};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("CarriedRequestEntry", 203);
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&{
                let entry =
                    Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
                let inventory = ArtifactInventory::default();
                let artifacts = inventory
                    .admit_shared_with_demand(
                        &inventory.empty_view(),
                        vec![entry.clone()],
                        crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                    )
                    .unwrap();
                let projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
                let mut validation = PackageInterfaceValidation::default();
                let mut selection =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &projection,
                        &artifacts.metadata_snapshot(),
                        &validation.inventory,
                    )
                    .unwrap();
                let output = crate::declaration_context::CapturedCompilerStage::issue(
                    &mut selection,
                    &artifacts,
                    Vec::new(),
                    &mut validation,
                )
                .unwrap();
                crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
                    output.compiler_inputs(),
                )
                .unwrap()
            })
            .unwrap(),
    );
    let scratch = tempfile::tempdir().unwrap();
    let request = context
        .prepare_compilation(scratch.path(), PRODUCER)
        .unwrap();
    let directory = request
        .materialization
        .as_ref()
        .unwrap()
        ._directory
        .as_ref()
        .unwrap();
    let directory_owner = Arc::downgrade(directory);
    let directory_path = directory.path().to_path_buf();
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    let inputs = request.compiler_inputs();
    let projection = inputs.projection().clone();
    let operation = tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs, &operation,
    )
    .unwrap();
    let view = inputs.artifacts().clone();
    assert!(selection
        .complete_native_entries(&view, &operation)
        .unwrap()
        .is_empty());
    let entry = view
        .metadata_snapshot()
        .artifacts
        .values()
        .find(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        .cloned()
        .unwrap();
    let id = entry.descriptor.id;
    let entry_owner = Arc::downgrade(&entry);
    assert!(Arc::ptr_eq(
        &entry,
        &view.metadata_snapshot().artifacts[&id]
    ));

    let raw_selection =
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &view.metadata_snapshot(),
            &operation,
        )
        .unwrap();
    assert!(matches!(
        raw_selection.complete_native_entries(&ArtifactInventory::default().empty_view(), &operation),
        Err(crate::certified_products::CertificationError::MissingRetainedCustody { artifact })
            if artifact == id
    ));
    drop(raw_selection);

    // Equal payloads in a separately issued scope do not supply this request's binding.
    let foreign_inventory = ArtifactInventory::default();
    let foreign_entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let marker = Arc::new(
        ArtifactEntry::original(producer.sha256(), product("ForeignMarker", 204)).unwrap(),
    );
    let foreign = foreign_inventory
        .admit_shared_with_demand(
            &foreign_inventory.empty_view(),
            vec![foreign_entry.clone(), marker],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    assert_eq!(entry.descriptor.id, foreign_entry.descriptor.id);
    assert!(!Arc::ptr_eq(&entry, &foreign_entry));
    assert_ne!(
        view.selection_snapshot().namespace.get(&id),
        foreign.selection_snapshot().namespace.get(&id)
    );
    let before = view.capture_graph_selection();

    let mut reference_validation = PackageInterfaceValidation::default();
    let calls = original_entry_construction_calls();
    let reference = ArtifactEntry::original_with_validation(
        producer.sha256(),
        native.clone(),
        &mut reference_validation,
    )
    .unwrap();
    assert_eq!(
        original_entry_construction_calls() - calls,
        1,
        "old construction work is observable even if inventory later interns equal facts"
    );
    assert_eq!(*entry, reference);
    let calls = original_entry_construction_calls();
    let issued = OriginalCompilerInputs::from_issued_support(inputs.compiler_source_support());
    assert_eq!(original_entry_construction_calls() - calls, 0);
    assert!(Arc::ptr_eq(
        &entry,
        &issued.artifacts.metadata_snapshot().artifacts[&id]
    ));
    assert_eq!(view.capture_graph_selection(), before);
    assert_eq!(
        issued.artifacts.selection_snapshot().namespace.get(&id),
        view.selection_snapshot().namespace.get(&id)
    );
    assert!(issued.artifacts.selected_native_groups().is_empty());
    let availability = issued
        .native_availability
        .rows()
        .find(|row| row.original.descriptor.id == id)
        .unwrap();
    assert_eq!(availability.mode, NativeTransportMode::Full);
    assert!(availability.permits(7));
    assert!(availability.permits(9001));
    let expected_edges = view.binding_dependencies();
    assert!(!expected_edges.is_empty());
    assert!(expected_edges
        .iter()
        .all(|edge| issued.artifacts.binding_dependencies().contains(edge)));

    let fresh_output_calls = original_entry_construction_calls();
    (|| -> Result<crate::declaration_context::CapturedCompilerStage, CompileError> {
        let fixture_products: &[CertifiedRecoveryProduct] = &[native.clone()];
        let entries = fixture_products
            .iter()
            .map(|product| {
                ArtifactEntry::original(producer.sha256(), product.clone()).map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let inventory = ArtifactInventory::default();
        let artifacts = inventory.admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )?;
        let projection = CompilerInputProjection::from_issued_entries(&entries)?;
        let mut validation = PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
        crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            &artifacts,
            Vec::new(),
            &mut validation,
        )
    })()
    .unwrap();
    assert!(
        original_entry_construction_calls() > fresh_output_calls,
        "independently selected fresh output retains ordinary construction"
    );

    drop(reference);
    drop(reference_validation);
    drop(entry);
    drop(native);
    drop(foreign_entry);
    drop(foreign);
    drop(foreign_inventory);
    drop(selection);
    drop(projection);
    drop(view);
    drop(request);
    drop(context);
    assert!(
        entry_owner.upgrade().is_some(),
        "the issued request retains the actual entry owner"
    );
    assert!(
        directory_owner.upgrade().is_some(),
        "the issued view retains physical materialization custody"
    );
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    drop(issued);
    assert!(entry_owner.upgrade().is_none());
    assert!(directory_owner.upgrade().is_none());
    assert!(!directory_path.try_exists().unwrap());
}
