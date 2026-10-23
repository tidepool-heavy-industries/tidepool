//! Protected native candidates are retained as private source availability,
//! independently of the optional manifest offer.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn select(
    scratch: &Path,
    records: Vec<Record>,
    context: &ExactCandidateContext,
) -> Option<CandidateSet> {
    super::select_records_inner(
        b"endpoint",
        &[],
        scratch,
        records
            .into_iter()
            .map(|record| {
                let origin = CandidateOrigin::Deployment;
                (record, origin)
            })
            .collect(),
        Some(context),
    )
}

fn context_for(
    records: &[Record],
    protected: &BTreeSet<(String, String)>,
) -> ExactCandidateContext {
    let canonical = records
        .iter()
        .filter(|record| protected.contains(&(record.unit.clone(), record.module.clone())))
        .map(|record| {
            (
                (record.unit.clone(), record.module.clone()),
                record.module_interface_proof.as_ref().unwrap().clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    ExactCandidateContext::new(protected.clone(), BTreeSet::new())
        .with_canonical_interfaces(canonical, BTreeSet::new())
}

fn original_draft(record: &Record) -> crate::recovery_artifacts::OriginalDraft {
    crate::recovery_artifacts::OriginalDraft::from_certification(
        computed_owner(record),
        record.interface.clone(),
        record.products.clone(),
        record.package_imports.clone(),
        record.original_certification.clone(),
    )
    .with_module_interface(record.module_interface_proof.as_ref().unwrap().clone())
    .unwrap()
}

fn original_product(record: &Record) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    crate::certified_products::certify_original_draft_with_validation(
        original_draft(record),
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
    .unwrap()
}

fn assert_empty_offer(selected: CandidateSet) {
    assert!(selected.by_owner.is_empty());
    assert!(selected.contextual_native_availability.is_empty());
    let manifest: Value =
        ciborium::de::from_reader(fs::File::open(&selected.manifest_path).unwrap()).unwrap();
    assert!(manifest.as_array().unwrap()[4]
        .as_array()
        .unwrap()
        .is_empty());
}

fn same_canonical_facts(
    left: &crate::certified_products::CertifiedModuleInterface,
    right: &crate::certified_products::CertifiedModuleInterface,
) -> bool {
    left.producer_sha256() == right.producer_sha256()
        && left.requirements() == right.requirements()
        && left.certificate_bytes() == right.certificate_bytes()
        && left.interface_bytes() == right.interface_bytes()
        && left.package_imports_bytes() == right.package_imports_bytes()
        && left.core_bytes() == right.core_bytes()
}

#[test]
fn compatible_protected_original_is_private_and_absent_from_manifest() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "Protected");
    let key = (record.unit.clone(), record.module.clone());
    let canonical = record.module_interface_proof.as_ref().unwrap().clone();
    let context = ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
        .with_originals(vec![original_product(&record)])
        .with_canonical_interfaces(BTreeMap::from([(key.clone(), canonical)]), BTreeSet::new());

    let selected = select(scratch.path(), vec![record.clone()], &context).unwrap();

    assert!(selected.by_owner.is_empty());
    assert_eq!(selected.contextual_native_availability.len(), 1);
    assert_eq!(
        selected.contextual_native_availability[0].owner(),
        &computed_owner(&record)
    );
    assert_eq!(
        selected.contextual_native_availability[0].product_bytes(),
        record.products
    );
    let manifest: Value =
        ciborium::de::from_reader(fs::File::open(&selected.manifest_path).unwrap()).unwrap();
    let fields = manifest.as_array().unwrap();
    assert!(fields[4].as_array().unwrap().is_empty());
}

#[test]
fn protected_catalog_availability_preserves_receiving_interface_only_selection() {
    use crate::artifact_inventory::{
        ArtifactEntry, ArtifactInventory, CanonicalProducerIdentity, CompilerInputProjection,
        NativeArtifactDemand,
    };
    use crate::declaration_context::{
        ExactCompileContext, ExactDeclarationContext, OriginalCompilerInputs,
    };
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "ProtectedReceiver");
    let canonical = record.module_interface_proof.as_ref().unwrap().clone();
    let entry = Arc::new(ArtifactEntry::canonical(canonical.clone()));
    let producer = CanonicalProducerIdentity::from_producer_bytes(&record.endpoint);
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let baseline = Arc::new(
        ExactDeclarationContext::from_authenticated_interfaces(
            producer.sha256(),
            &artifacts,
            CompilerInputProjection::from_issued_entries(&[entry]).unwrap(),
        )
        .unwrap(),
    );
    let key = (record.unit.clone(), record.module.clone());
    let context = ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
        .with_canonical_interfaces(
            BTreeMap::from([(key.clone(), canonical.clone())]),
            BTreeSet::new(),
        )
        .with_interface_seals(BTreeMap::from([(key, canonical.interface_sha256())]));
    let selected = select(scratch.path(), vec![record.clone()], &context).unwrap();
    assert!(selected.by_owner.is_empty());
    assert_eq!(selected.contextual_native_availability.len(), 1);
    let original = ArtifactEntry::original(
        producer.sha256(),
        selected.contextual_native_availability[0].clone(),
    )
    .unwrap()
    .descriptor
    .id;
    let support = baseline.compiler_source_support();
    let received = OriginalCompilerInputs::from_candidates(&baseline, producer, &selected)
        .unwrap()
        .unwrap();
    assert_eq!(received.role_projection(), support.projection());
    assert_eq!(
        received.native_availability(),
        support.native_availability()
    );
    assert_eq!(received.issued_offers(), support.native_offers());
    assert_eq!(
        received
            .issued_input_decisions()
            .map(|decisions| decisions.recorded_facts()),
        Some(support.input_decisions().recorded_facts())
    );
    assert_eq!(
        received
            .issued_artifacts()
            .capture_graph_selection()
            .namespace,
        support.artifacts().capture_graph_selection().namespace
    );
    assert!(received
        .issued_artifacts()
        .artifact_ids()
        .contains(&original));
    let request = ExactCompileContext::new(baseline.clone())
        .prepare_compilation_with_private_input(
            scratch.path(),
            &record.endpoint,
            None,
            Some(received),
        )
        .unwrap();
    assert_eq!(request.compiler_inputs().projection(), support.projection());
    assert!(request
        .compiler_inputs()
        .native_availability(original)
        .is_none());
    assert_eq!(
        request
            .compiler_inputs()
            .artifacts()
            .capture_graph_selection()
            .namespace,
        support.artifacts().capture_graph_selection().namespace
    );

    // Reconstructing the valid owner packet must reach the receiver before
    // perturbing one field at its enforcing canonical/native binding boundary.
    let product = &selected.contextual_native_availability[0];
    let reconstruct = |certification, packages| {
        let draft = crate::recovery_artifacts::OriginalDraft::from_certification(
            product.owner().clone(),
            product.interface_bytes().to_vec(),
            product.product_bytes().to_vec(),
            packages,
            certification,
        )
        .with_module_interface(canonical.clone())
        .map_err(crate::certified_products::CertificationError::CapturedModulePayload)?;
        crate::certified_products::certify_original_draft_with_validation(
            draft,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
    };
    let valid = reconstruct(
        product.certification_bytes().to_vec(),
        product.package_imports_bytes().to_vec(),
    )
    .expect("unchanged fixture bytes bind to their original canonical companion");
    let reconstructed = CandidateSet {
        manifest_path: selected.manifest_path.clone(),
        by_owner: BTreeMap::new(),
        contextual_native_availability: vec![valid],
        catalog_owner: None,
        input_transport: Vec::new(),
    };
    assert!(OriginalCompilerInputs::from_candidates(&baseline, producer, &reconstructed).is_ok());
    for (label, certification, packages) in [
        (
            "certification",
            vec![0xff],
            product.package_imports_bytes().to_vec(),
        ),
        (
            "packages",
            product.certification_bytes().to_vec(),
            vec![0xff],
        ),
    ] {
        assert!(
            reconstruct(certification, packages).is_err(),
            "invalid {label} must refuse at the canonical/native binding owner"
        );
    }
}

#[test]
fn ambiguous_protected_owner_requires_an_explicit_selected_original() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "Ambiguous");
    let key = (record.unit.clone(), record.module.clone());
    let canonical = record.module_interface_proof.as_ref().unwrap().clone();
    let context = ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
        .with_canonical_interfaces(
            BTreeMap::from([(key.clone(), canonical)]),
            BTreeSet::from([key]),
        );

    assert_empty_offer(select(scratch.path(), vec![record], &context).unwrap());
}

#[test]
fn reserved_protected_owner_cannot_become_native_availability() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "Generated");
    let key = (record.unit.clone(), record.module.clone());
    let canonical = record.module_interface_proof.as_ref().unwrap().clone();
    let context = ExactCandidateContext::new(
        BTreeSet::from([key.clone()]),
        BTreeSet::from([key.1.clone()]),
    )
    .with_canonical_interfaces(BTreeMap::from([(key.clone(), canonical)]), BTreeSet::new());

    assert_empty_offer(select(scratch.path(), vec![record], &context).unwrap());
}

#[test]
fn same_interface_with_different_core_is_refused_by_full_selection() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "Protected");
    let key = (record.unit.clone(), record.module.clone());
    let canonical = record.module_interface_proof.as_ref().unwrap().clone();
    let changed_core =
        crate::certified_products::fixture_module_core(&canonical, b"changed-core".to_vec());

    assert_eq!(changed_core.producer_sha256(), canonical.producer_sha256());
    assert_eq!(changed_core.source_sha256(), canonical.source_sha256());
    assert_eq!(changed_core.requirements(), canonical.requirements());
    assert_eq!(changed_core.interface_bytes(), canonical.interface_bytes());
    assert_eq!(
        changed_core.package_imports_bytes(),
        canonical.package_imports_bytes()
    );
    assert_ne!(changed_core.core_bytes(), canonical.core_bytes());

    let context = ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
        .with_originals(vec![original_product(&record)])
        .with_canonical_interfaces(BTreeMap::from([(key, changed_core)]), BTreeSet::new());

    assert_empty_offer(select(scratch.path(), vec![record], &context).unwrap());
}

fn property_config(
    cases: u32,
    seed: u64,
    test_name: &'static str,
) -> proptest::test_runner::Config {
    use proptest::test_runner::{Config, FileFailurePersistence, RngSeed};
    let mut config = Config::default();
    config.cases = cases;
    config.rng_seed = RngSeed::Fixed(seed);
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(test_name);
    config
}

#[test]
fn generated_protected_native_refusals_match_complete_canonical_and_owner_facts() {
    use proptest::prelude::*;
    use proptest::test_runner::TestRunner;

    // Re-finalizing the original product keeps its interface, package bytes,
    // and Core fixed while independently perturbing producer, source, or
    // canonical requirements. The candidate crosses the complete selector.
    let config = property_config(
        64,
        2026100808,
        concat!(
            module_path!(),
            "::generated_protected_native_refusals_match_complete_canonical_and_owner_facts"
        ),
    );
    let configuration = format!("{config:?}");
    let configured_cases = config.cases;
    let mut runner = TestRunner::new(config);
    let callbacks = std::cell::Cell::new(0usize);
    let completed = std::cell::Cell::new(0usize);
    let partitions = std::cell::RefCell::new([0usize; 7]);
    let result = runner.run(&(0u8..7), |variant| {
        callbacks.set(callbacks.get() + 1);
        partitions.borrow_mut()[usize::from(variant)] += 1;
        let sources = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let record = super::tests::candidate_fixture(sources.path(), "Protected");
        let key = (record.unit.clone(), record.module.clone());
        let canonical = record.module_interface_proof.as_ref().unwrap().clone();
        let producer = canonical.producer_sha256();
        let base_draft = original_draft(&record);
        let required = |canonical| {
            ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
                .with_originals(vec![original_product(&record)])
                .with_canonical_interfaces(
                    BTreeMap::from([(key.clone(), canonical)]),
                    BTreeSet::new(),
                )
        };

        let context = match variant {
            0 => required(canonical.clone()),
            1 => required(
                crate::certified_products::fixture_finalized_product_with_source(
                    base_draft.clone(),
                    producer,
                    [0x53; 32],
                )
                .module_interface()
                .clone(),
            ),
            2 => required(
                crate::certified_products::fixture_finalized_product(
                    base_draft.clone(),
                    [0xa7; 32],
                )
                .module_interface()
                .clone(),
            ),
            3 => required(
                crate::certified_products::fixture_finalized_product_with_requirements(
                    base_draft.clone(),
                    producer,
                    Some(BTreeMap::from([(
                        ("u".into(), "Dependency".into()),
                        [0x42; 32],
                    )])),
                )
                .module_interface()
                .clone(),
            ),
            4 => required(crate::certified_products::fixture_module_core(
                &canonical,
                b"changed-core".to_vec(),
            )),
            5 => required(crate::certified_products::fixture_source_module_interface(
                canonical.producer_sha256(),
                "other-unit",
                "OtherOwner",
                canonical.source_sha256(),
                canonical.requirements().clone(),
                None,
            )),
            6 => ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new()),
            _ => unreachable!(),
        };

        let expected = match context.canonical_interfaces.get(&key) {
            Some(selected) => same_canonical_facts(selected, &canonical),
            None => false,
        };
        let selected = select(scratch.path(), vec![record], &context).unwrap();
        if expected {
            prop_assert_eq!(selected.contextual_native_availability.len(), 1);
            prop_assert!(selected.by_owner.is_empty());
        } else {
            prop_assert!(selected.contextual_native_availability.is_empty());
            prop_assert!(selected.by_owner.is_empty());
        }
        completed.set(completed.get() + 1);
        Ok(())
    });
    eprintln!(
        "protected_native_refusal_campaign={}",
        serde_json::json!({
            "configured_fresh_cases": configured_cases,
            "configuration": configuration,
            "callbacks": callbacks.get(),
            "completed_callbacks": completed.get(),
            "variant_callbacks": *partitions.borrow(),
            "variant_order": ["compatible", "source", "producer", "requirements", "Core", "owner", "missing_canonical"],
        })
    );
    result.unwrap();
}

#[test]
fn protected_original_owner_version_must_match_the_selected_candidate() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "Protected");
    let key = (record.unit.clone(), record.module.clone());
    let canonical = record.module_interface_proof.as_ref().unwrap().clone();
    let mut wrong_owner = computed_owner(&record);
    wrong_owner.module_version.0[0] ^= 1;
    let wrong_original = crate::certified_products::fixture_finalized_product_with_source(
        crate::recovery_artifacts::OriginalDraft::from_certification(
            wrong_owner.clone(),
            record.interface.clone(),
            record.products.clone(),
            record.package_imports.clone(),
            crate::certified_products::encode_home_certification(
                &wrong_owner,
                &[],
                &BTreeMap::new(),
            )
            .unwrap(),
        ),
        canonical.producer_sha256(),
        parse_sha(&record.source_sha256).unwrap(),
    );
    assert_eq!(wrong_original.module_interface(), &canonical);
    let context = ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
        .with_originals(vec![wrong_original])
        .with_canonical_interfaces(BTreeMap::from([(key, canonical)]), BTreeSet::new());

    assert_empty_offer(select(scratch.path(), vec![record], &context).unwrap());
}

#[test]
fn protected_root_selection_matches_a_full_scan_oracle() {
    use proptest::prelude::*;
    use proptest::test_runner::TestRunner;

    let config = property_config(
        32,
        2026100809,
        concat!(
            module_path!(),
            "::protected_root_selection_matches_a_full_scan_oracle"
        ),
    );
    let configuration = format!("{config:?}");
    let configured_cases = config.cases;
    let mut runner = TestRunner::new(config);
    let callbacks = std::cell::Cell::new(0usize);
    let completed = std::cell::Cell::new(0usize);
    // all protected, all unprotected, mixed; generated owner order changed/unchanged.
    let partitions = std::cell::RefCell::new([0usize; 5]);
    let strategy = proptest::collection::vec((any::<bool>(), any::<u8>()), 1..7);
    let result = runner.run(&strategy, |history| {
        callbacks.set(callbacks.get() + 1);
        let private_count = history.iter().filter(|(private, _)| *private).count();
        let partition = if private_count == history.len() {
            0
        } else if private_count == 0 {
            1
        } else {
            2
        };
        partitions.borrow_mut()[partition] += 1;
        let selected = history
            .iter()
            .map(|(selected, _)| *selected)
            .collect::<Vec<_>>();
        let order = history.iter().map(|(_, order)| *order).collect::<Vec<_>>();
        let sources = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let records = (0..selected.len())
            .map(|index| super::tests::candidate_fixture(sources.path(), &format!("M{index}")))
            .collect::<Vec<_>>();
        let protected = selected
            .iter()
            .enumerate()
            .filter(|(_, selected)| **selected)
            .map(|(index, _)| ("u".to_owned(), format!("M{index}")))
            .collect::<BTreeSet<_>>();

        // The oracle scans the primary selection bits. It does not reuse
        // the production closure's traversal or retained availability.
        let expected_private = records
            .iter()
            .filter(|record| protected.contains(&(record.unit.clone(), record.module.clone())))
            .map(|record| computed_owner(record))
            .collect::<BTreeSet<_>>();
        let expected_manifest = records
            .iter()
            .map(|record| computed_owner(record))
            .filter(|owner| !protected.contains(&(owner.unit.clone(), owner.module.clone())))
            .collect::<BTreeSet<_>>();

        let context = context_for(&records, &protected);
        let mut indices = (0..records.len()).collect::<Vec<_>>();
        indices.sort_by_key(|index| order[*index]);
        let changed_order = indices
            .iter()
            .enumerate()
            .any(|(position, index)| position != *index);
        partitions.borrow_mut()[if changed_order { 3 } else { 4 }] += 1;
        let arranged = indices
            .into_iter()
            .map(|index| records[index].clone())
            .collect::<Vec<_>>();
        let selected = select(scratch.path(), arranged, &context).unwrap();
        let actual_private = selected
            .contextual_native_availability
            .iter()
            .map(|product| product.owner().clone())
            .collect::<BTreeSet<_>>();
        let actual_manifest = selected
            .by_owner
            .values()
            .map(|candidate| candidate.owner.clone())
            .collect::<BTreeSet<_>>();

        prop_assert_eq!(actual_private, expected_private);
        prop_assert_eq!(actual_manifest, expected_manifest);
        completed.set(completed.get() + 1);
        Ok(())
    });
    eprintln!(
        "protected_native_root_campaign={}",
        serde_json::json!({
            "configured_fresh_cases": configured_cases,
            "configuration": configuration,
            "callbacks": callbacks.get(),
            "completed_callbacks": completed.get(),
            "partition_callbacks": *partitions.borrow(),
            "partition_order": ["all_protected", "all_unprotected", "mixed", "reordered", "unchanged_order"],
        })
    );
    result.unwrap();
}

#[test]
fn protected_ordinary_record_remains_excluded_before_source_preflight() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "ProtectedOrdinary");
    let key = (record.unit.clone(), record.module.clone());
    let context = context_for(std::slice::from_ref(&record), &BTreeSet::from([key]));
    let selected = super::select_records_inner(
        b"endpoint",
        &[],
        scratch.path(),
        vec![(record, CandidateOrigin::Ordinary)],
        Some(&context),
    )
    .unwrap();
    assert_empty_offer(selected);
}

#[test]
fn protected_deployment_record_refuses_changed_source_and_competing_definitions() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let record = super::tests::candidate_fixture(sources.path(), "ProtectedSource");
    let key = (record.unit.clone(), record.module.clone());
    let context = context_for(std::slice::from_ref(&record), &BTreeSet::from([key]));
    let intact = select(scratch.path(), vec![record.clone()], &context).unwrap();
    assert!(intact.by_owner.is_empty());
    assert_eq!(intact.contextual_native_availability.len(), 1);
    assert!(select(
        scratch.path(),
        vec![record.clone(), record.clone()],
        &context
    )
    .is_none());
    std::fs::write(
        &record.source,
        "module ProtectedSource where\nchanged = True\n",
    )
    .unwrap();
    assert!(select(scratch.path(), vec![record], &context).is_none());
}

#[test]
fn protected_native_closure_issues_missing_type_dependencies_privately() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut dependent = super::tests::candidate_fixture(sources.path(), "Dependent");
    let required = super::tests::candidate_fixture(sources.path(), "TypeSupport");
    let required_key = (required.unit.clone(), required.module.clone());
    let canonical = crate::certified_products::fixture_finalized_product_with_requirements(
        original_draft(&dependent),
        dependent
            .module_interface_proof
            .as_ref()
            .unwrap()
            .producer_sha256(),
        Some(BTreeMap::from([(
            required_key.clone(),
            computed_owner(&required).skinny_iface_sha256,
        )])),
    );
    dependent.original_certification = canonical.certification_bytes().to_vec();
    dependent.module_interface_proof = Some(canonical.module_interface().clone());
    dependent.module_interface = Some(
        crate::recovery_artifacts::materialize_module_interface(
            sources.path(),
            canonical.module_interface(),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            crate::recovery_artifacts::MaterializationMode::Durable,
        )
        .unwrap(),
    );
    let key = (dependent.unit.clone(), dependent.module.clone());
    let context =
        context_for(&[dependent.clone()], &BTreeSet::from([key.clone()])).with_interface_seals(
            BTreeMap::from([(key, computed_owner(&dependent).skinny_iface_sha256)]),
        );
    let selected = select(
        scratch.path(),
        vec![dependent.clone(), required.clone()],
        &context,
    )
    .unwrap();
    assert!(selected.by_owner.is_empty());
    assert_eq!(
        selected
            .contextual_native_availability
            .iter()
            .map(|product| product.owner().module.as_str())
            .collect::<Vec<_>>(),
        vec!["Dependent", "TypeSupport"]
    );
    // A missing or incompatible canonical dependency refuses the private offer.
    assert_empty_offer(select(scratch.path(), vec![dependent], &context).unwrap());
}
