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
                let origin = CandidateOrigin::Deployment {
                    interface: record.source.clone(),
                    packages: record.source.clone(),
                };
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

fn original_product(record: &Record) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
        computed_owner(record),
        record.interface.clone(),
        record.products.clone(),
        record.package_imports.clone(),
        record.original_certification.clone(),
    )
    .with_source_sha256(parse_sha(&record.source_sha256).unwrap())
    .with_module_interface(record.module_interface_proof.as_ref().unwrap().clone())
    .unwrap()
}

fn assert_empty_offer(selected: CandidateSet) {
    assert!(selected.by_owner.is_empty());
    assert!(selected.native_availability.is_empty());
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
    assert_eq!(selected.native_availability.len(), 1);
    assert_eq!(
        selected.native_availability[0].owner(),
        &computed_owner(&record)
    );
    assert_eq!(
        selected.native_availability[0].product_bytes(),
        record.products
    );
    let manifest: Value =
        ciborium::de::from_reader(fs::File::open(&selected.manifest_path).unwrap()).unwrap();
    let fields = manifest.as_array().unwrap();
    assert!(fields[4].as_array().unwrap().is_empty());
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

#[test]
fn generated_protected_native_refusals_match_complete_canonical_and_owner_facts() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, RngSeed, TestRunner};

    // Re-finalizing the original product keeps its interface, package bytes,
    // and Core fixed while independently perturbing producer, source, or
    // canonical requirements. The candidate crosses the complete selector.
    let mut config = Config::default();
    config.cases = 64;
    config.rng_seed = RngSeed::Fixed(2026100808);
    config.failure_persistence = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS").map(|path| {
        Box::new(FileFailurePersistence::Direct(path))
            as Box<dyn proptest::test_runner::FailurePersistence>
    });
    let mut runner = TestRunner::new(config);
    runner
        .run(&(0u8..7), |variant| {
            let sources = tempfile::tempdir().unwrap();
            let scratch = tempfile::tempdir().unwrap();
            let record = super::tests::candidate_fixture(sources.path(), "Protected");
            let key = (record.unit.clone(), record.module.clone());
            let canonical = record.module_interface_proof.as_ref().unwrap().clone();
            let producer = canonical.producer_sha256();
            let base_product = original_product(&record);
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
                    crate::certified_products::fixture_finalized_product(
                        base_product.clone().with_source_sha256([0x53; 32]),
                        producer,
                    )
                    .module_interface()
                    .unwrap()
                    .clone(),
                ),
                2 => required(
                    crate::certified_products::fixture_finalized_product(
                        base_product.clone(),
                        [0xa7; 32],
                    )
                    .module_interface()
                    .unwrap()
                    .clone(),
                ),
                3 => required(
                    crate::certified_products::fixture_finalized_product_with_requirements(
                        base_product.clone(),
                        producer,
                        Some(BTreeMap::from([(
                            ("u".into(), "Dependency".into()),
                            [0x42; 32],
                        )])),
                    )
                    .module_interface()
                    .unwrap()
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
                prop_assert_eq!(selected.native_availability.len(), 1);
                prop_assert!(selected.by_owner.is_empty());
            } else {
                prop_assert!(selected.native_availability.is_empty());
                prop_assert!(selected.by_owner.is_empty());
            }
            Ok(())
        })
        .unwrap();
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
    let wrong_original = crate::certified_products::fixture_finalized_product(
        crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
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
        )
        .with_source_sha256(parse_sha(&record.source_sha256).unwrap()),
        canonical.producer_sha256(),
    );
    assert_eq!(wrong_original.module_interface(), Some(&canonical));
    let context = ExactCandidateContext::new(BTreeSet::from([key.clone()]), BTreeSet::new())
        .with_originals(vec![wrong_original])
        .with_canonical_interfaces(BTreeMap::from([(key, canonical)]), BTreeSet::new());

    assert_empty_offer(select(scratch.path(), vec![record], &context).unwrap());
}

#[test]
fn protected_root_selection_matches_a_full_scan_oracle() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, RngSeed, TestRunner};

    let mut config = Config::default();
    config.cases = 32;
    config.rng_seed = RngSeed::Fixed(2026100809);
    config.failure_persistence = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS").map(|path| {
        Box::new(FileFailurePersistence::Direct(path))
            as Box<dyn proptest::test_runner::FailurePersistence>
    });
    let mut runner = TestRunner::new(config);
    let strategy = proptest::collection::vec((any::<bool>(), any::<u8>()), 1..7);
    runner
        .run(&strategy, |history| {
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
            let arranged = indices
                .into_iter()
                .map(|index| records[index].clone())
                .collect::<Vec<_>>();
            let selected = select(scratch.path(), arranged, &context).unwrap();
            let actual_private = selected
                .native_availability
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
            Ok(())
        })
        .unwrap();
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
    assert_empty_offer(select(scratch.path(), vec![record], &context).unwrap());
}

#[test]
fn protected_native_closure_issues_missing_type_dependencies_privately() {
    let sources = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut dependent = super::tests::candidate_fixture(sources.path(), "Dependent");
    let required = super::tests::candidate_fixture(sources.path(), "TypeSupport");
    let required_key = (required.unit.clone(), required.module.clone());
    let canonical = crate::certified_products::fixture_finalized_product_with_requirements(
        original_product(&dependent),
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
    dependent.module_interface_proof = canonical.module_interface().cloned();
    dependent.module_interface = Some(
        crate::recovery_artifacts::materialize_module_interface(
            super::tests::fixture_record_dir(sources.path())
                .parent()
                .unwrap(),
            canonical.module_interface().unwrap(),
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
            .native_availability
            .iter()
            .map(|product| product.owner().module.as_str())
            .collect::<Vec<_>>(),
        vec!["Dependent", "TypeSupport"]
    );
    // A missing or incompatible canonical dependency refuses the private offer.
    assert_empty_offer(select(scratch.path(), vec![dependent], &context).unwrap());
}
