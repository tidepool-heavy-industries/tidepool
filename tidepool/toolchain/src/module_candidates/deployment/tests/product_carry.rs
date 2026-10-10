use super::*;
use crate::module_candidates::product_decode_observer::DecodeCounter;
use tidepool_repr::execution_schema::RawModuleProduct;

#[test]
fn deployment_evidence_sharing_reauthenticates_every_file_before_reusing_a_proof() {
    let fixture = Fixture::with_modules(&["A", "B"]);
    let catalog = fixture.catalog();
    assert_eq!(
        catalog.modules[0].evidence.sha256,
        catalog.modules[1].evidence.sha256
    );
    assert_eq!(
        catalog.modules[0].evidence.path,
        catalog.modules[1].evidence.path
    );
    let changed = fixture.output.join(&catalog.modules[0].evidence.path);
    let original = fs::read(&changed).unwrap();
    fs::write(&changed, b"changed duplicate physical evidence file").unwrap();
    assert!(
        matches!(fixture.load(), Err(ModulePackageError::ArtifactChanged(path)) if path == changed)
    );
    fs::write(changed, original).unwrap();
    let package = fixture.load().unwrap();
    assert!(std::ptr::eq(
        &*package.records[0].evidence,
        &*package.records[1].evidence
    ));
    let work = package
        .revalidate(&fixture.output.join("catalog.json"), RootPolicy::Fixture)
        .unwrap();
    let distinct_evidence_paths = catalog
        .modules
        .iter()
        .map(|module| &module.evidence.path)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(distinct_evidence_paths, 1);
    assert_eq!(
        fs::read_dir(fixture.output.join("evidence"))
            .unwrap()
            .count(),
        1,
        "producer writes one physical blob for the shared proof"
    );
    assert_eq!(
        work.artifact_files,
        1 + catalog.execution_graphs.len() as u64
            + (catalog.modules.len() * 5) as u64
            + distinct_evidence_paths as u64
            + catalog
                .modules
                .iter()
                .map(|module| 3 + usize::from(module.module_interface.core.is_some()))
                .sum::<usize>() as u64
    );
    assert_eq!(work.evidence.source_read_attempts, 2);
    assert_eq!(
        work.evidence.source_read_bytes,
        package.records[0]
            .evidence
            .sources
            .iter()
            .filter(|source| source.path != Path::new("@generated-source"))
            .map(|source| fs::metadata(&source.path).unwrap().len())
            .sum::<u64>()
    );
}

#[test]
fn deployment_export_writes_one_evidence_blob_for_78_module_rows() {
    let names = (0..78)
        .map(|index| format!("Owner{index:03}"))
        .collect::<Vec<_>>();
    let fixture = Fixture::with_modules(&names.iter().map(String::as_str).collect::<Vec<_>>());
    let catalog = fixture.catalog();
    assert_eq!(catalog.modules.len(), 78);
    assert_eq!(
        catalog
            .modules
            .iter()
            .map(|module| (
                module.evidence.path.clone(),
                module.evidence.sha256.clone(),
                module.evidence.length
            ))
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1
    );
    assert_eq!(
        fs::read_dir(fixture.output.join("evidence"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn deployment_evidence_sharing_keeps_distinct_authenticated_wire_proofs_separate() {
    let fixture = Fixture::with_modules(&["A", "B"]);
    let mut catalog = fixture.catalog();
    let evidence = &mut catalog.modules[1].evidence;
    let original_path = fixture.output.join(&evidence.path);
    let original = fs::read(&original_path).unwrap();
    let altered = [original.as_slice(), b"\n"].concat();
    let decoded_original: crate::cache::DependencyEvidence =
        serde_json::from_slice(&original).unwrap();
    let decoded_altered: crate::cache::DependencyEvidence =
        serde_json::from_slice(&altered).unwrap();
    assert_eq!(
        serde_json::to_value(decoded_original).unwrap(),
        serde_json::to_value(decoded_altered).unwrap()
    );
    evidence.path = PathBuf::from("evidence/alternate-dependencies.json");
    let path = fixture.output.join(&evidence.path);
    fs::write(path, &altered).unwrap();
    evidence.length = altered.len() as u64;
    evidence.sha256 = sha(&altered);
    assert_ne!(
        catalog.modules[0].evidence.sha256,
        catalog.modules[1].evidence.sha256
    );
    fs::write(
        fixture.output.join("catalog.json"),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    let package = fixture.load().unwrap();
    assert!(!std::ptr::eq(
        &*package.records[0].evidence,
        &*package.records[1].evidence
    ));
    let work = package
        .revalidate(&fixture.output.join("catalog.json"), RootPolicy::Fixture)
        .unwrap();
    assert_eq!(work.evidence.source_read_attempts, 4);
}

#[test]
fn deployment_evidence_same_digest_at_distinct_paths_is_authenticated_per_path() {
    let fixture = Fixture::with_modules(&["A", "B"]);
    let mut catalog = fixture.catalog();
    let original = catalog.modules[0].evidence.clone();
    let original_path = fixture.output.join(&original.path);
    let original_bytes = fs::read(&original_path).unwrap();
    let alternate = PathBuf::from("evidence/second-copy.json");
    fs::write(fixture.output.join(&alternate), &original_bytes).unwrap();
    catalog.modules[1].evidence.path = alternate.clone();
    fs::write(
        fixture.output.join("catalog.json"),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();

    let package = fixture.load().unwrap();
    let baseline = package
        .revalidate(&fixture.output.join("catalog.json"), RootPolicy::Fixture)
        .unwrap();
    assert_eq!(
        baseline.artifact_files,
        1 + 2 * 5
            + 2
            + catalog
                .modules
                .iter()
                .map(|module| 3 + usize::from(module.module_interface.core.is_some()))
                .sum::<usize>() as u64
    );

    let alternate_path = fixture.output.join(&alternate);
    fs::write(&alternate_path, b"changed second physical evidence blob").unwrap();
    assert!(matches!(
        package.revalidate(&fixture.output.join("catalog.json"), RootPolicy::Fixture),
        Err(ModulePackageError::ArtifactChanged(path)) if path == alternate_path
    ));
    assert!(matches!(
        fixture.load(),
        Err(ModulePackageError::ArtifactChanged(path)) if path == alternate_path
    ));
    fs::write(alternate_path, original_bytes).unwrap();
    assert!(
        fixture.load().is_ok(),
        "fresh acquisition succeeds after restore"
    );
    assert!(package
        .revalidate(&fixture.output.join("catalog.json"), RootPolicy::Fixture)
        .is_ok());
}

#[test]
fn deployment_evidence_reference_graph_rejects_metadata_conflicts_and_aliases() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, RngSeed, TestRunner};

    let mut config = Config::default();
    config.cases = 24;
    config.rng_seed = RngSeed::Fixed(2026101001);
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::deployment_evidence_reference_graph_rejects_metadata_conflicts_and_aliases"
    ));
    let mut runner = TestRunner::new(config);
    let result = runner.run(&proptest::collection::vec(0u8..5, 1..8), |actions| {
        let fixture = Fixture::with_modules(&["A", "B"]);
        let package = fixture.load().unwrap();
        let catalog_path = fixture.output.join("catalog.json");
        let original_catalog = fs::read(&catalog_path).unwrap();
        let original = fixture.catalog();
        for action in actions {
            let mut changed = original.clone();
            let products_path = changed.modules[0].products.path.clone();
            let evidence = &mut changed.modules[1].evidence;
            match action {
                0 => evidence.sha256 = "0".repeat(64),
                1 => evidence.length = evidence.length.saturating_add(1),
                2 => evidence.path = products_path,
                3 => evidence.path = PathBuf::from("../outside.json"),
                _ => evidence.path = PathBuf::from("/absolute/evidence.json"),
            }
            fs::write(&catalog_path, serde_json::to_vec(&changed).unwrap()).unwrap();
            prop_assert!(matches!(fixture.load(), Err(ModulePackageError::Format(_))));

            fs::write(&catalog_path, &original_catalog).unwrap();
            prop_assert!(package
                .revalidate(&catalog_path, RootPolicy::Fixture)
                .is_ok());
        }
        Ok(())
    });
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn borrowed_shared_deployment_evidence_rechecks_protected_negative_witnesses() {
    let external = tempfile::tempdir().unwrap();
    let shadow = fs::canonicalize(external.path()).unwrap().join("A.hs");
    let fixture = Fixture::with_modules_execution_packages_and_shadow(
        &["A", "B"],
        false,
        &Default::default(),
        Some(&shadow),
    );
    let counter = DecodeCounter::new();
    let package = Arc::new(fixture.load().unwrap());
    assert!(!package.source_selection().contains_source(&shadow));
    assert!(std::ptr::eq(
        &*package.records[0].evidence,
        &*package.records[1].evidence
    ));
    borrow_catalog(&package).unwrap();
    fs::write(&shadow, b"module A where\nvalue = 8\n").unwrap();
    assert!(borrow_catalog(&package).is_err());
    fs::remove_file(shadow).unwrap();
    borrow_catalog(&package).unwrap();
    assert_eq!(counter.count(), 2);
}

#[test]
fn deployment_hydration_decodes_once_and_selection_carries_exact_products() {
    let names = ["A", "B"];
    let fixture = Fixture::with_modules(&names);
    let counter = DecodeCounter::new();
    let package = fixture.load().unwrap();
    assert_eq!(counter.count(), names.len());

    let expected = names
        .iter()
        .map(|name| {
            let product = RawModuleProduct {
                unit: "u".into(),
                module: (*name).into(),
                interface: name.as_bytes().to_vec(),
                groups: vec![],
            };
            let record = package
                .records
                .iter()
                .find(|record| record.record().module == *name)
                .unwrap();
            assert_eq!(*record.product, product);
            assert_eq!(
                record.record().products,
                product_bytes("u", name, name.as_bytes())
            );
            (
                product,
                record.record().products.clone(),
                record.record().original_owner.owner(),
            )
        })
        .collect::<Vec<_>>();

    let scratch = tempfile::tempdir().unwrap();
    let mut include = vec![scratch.path().to_path_buf()];
    include.extend(package.source_selection().include_roots());
    let candidates = package.candidates(&[3; 32]).unwrap();
    assert_eq!(counter.count(), names.len());
    let selected = super::super::super::select_records_inner(
        &[3; 32],
        &include,
        scratch.path(),
        candidates,
        None,
    )
    .unwrap();
    assert_eq!(counter.count(), names.len());
    assert_eq!(selected.by_owner.len(), names.len());
    for (product, bytes, owner) in expected {
        let bundle = &selected.by_owner[&(product.unit.clone(), product.module.clone())];
        assert_eq!(bundle.product.bytes(), bytes);
        assert_eq!(bundle.product.decoded(), &product);
        assert_eq!(bundle.owner, owner);
    }
}

#[test]
fn fresh_deployment_offers_redecode_and_refuse_changed_product_files() {
    let fixture = Fixture::new();
    let counter = DecodeCounter::new();
    let first = fixture.load().unwrap();
    assert_eq!(counter.count(), 1);
    let original = first.records[0].record().products.clone();
    let first_owner = first.records[0].record().original_owner.owner();
    drop(first);

    let second = fixture.load().unwrap();
    assert_eq!(counter.count(), 2);
    assert_eq!(second.records[0].record().products, original);
    assert_eq!(
        second.records[0].record().original_owner.owner(),
        first_owner
    );
    drop(second);

    let reference = fixture.catalog().modules[0].products.clone();
    let path = fixture.output.join(reference.path);
    let mut corrupt = original.clone();
    corrupt.push(0);
    fs::write(&path, corrupt).unwrap();
    assert!(matches!(
        fixture.load(),
        Err(ModulePackageError::ArtifactChanged(changed)) if changed == path
    ));
    assert_eq!(
        counter.count(),
        2,
        "changed file is refused before product decoding"
    );

    fs::write(path, &original).unwrap();
    let restored = fixture.load().unwrap();
    assert_eq!(counter.count(), 3);
    assert_eq!(restored.records[0].record().products, original);
    assert_eq!(
        restored.records[0].record().original_owner.owner(),
        first_owner
    );
}

#[test]
fn deployment_singleton_decode_refuses_invalid_framing() {
    let fixture = Fixture::new();
    let package = fixture.load().unwrap();
    let record = package.records[0].record().clone();
    let product = (*package.records[0].product).clone();
    let mut trailing = record.products.clone();
    trailing.push(0);
    let mut other = product.clone();
    other.module = "Other".into();
    let cases = [
        b"not CBOR".to_vec(),
        trailing,
        tidepool_test_data::prepared_encode::encode_module_products(&[]),
        tidepool_test_data::prepared_encode::encode_module_products(&[product, other]),
    ];
    let counter = DecodeCounter::new();
    for (index, products) in cases.into_iter().enumerate() {
        let mut invalid = record.clone();
        invalid.data.products = products;
        assert!(matches!(
            DecodedDeploymentRecord::decode(invalid, &InventoryOperation::new(Default::default()),),
            Err(ModulePackageError::Format(_))
        ));
        assert_eq!(counter.count(), index + 1);
    }
}

fn borrow_catalog(
    package: &Arc<DeploymentModulePackage>,
) -> Result<super::super::super::CandidateSet, ModulePackageError> {
    let scratch = tempfile::tempdir().unwrap();
    super::super::super::select_acquired_catalog(
        package,
        &[3; 32],
        &package.source_selection().include_roots(),
        scratch.path(),
    )
    .map(|selected| selected.expect("complete fixture cohort"))
}

#[test]
fn acquired_catalog_borrows_originals_and_detached_views_keep_their_paths() {
    use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory};
    let fixture = Fixture::with_modules_and_execution(&["Library"], true);
    let counter = DecodeCounter::new();
    let package = Arc::new(fixture.load().unwrap());
    let weak = Arc::downgrade(&package);
    let selected = borrow_catalog(&package).unwrap();
    let original = &selected.by_owner[&("u".into(), "Library".into())];
    let private_interface = original.iface_path.clone();
    let expected_interface = fs::read(&private_interface).unwrap();
    let custody = original
        .original_module_interface
        .catalog_input_custody()
        .unwrap();
    let canonical_path = custody
        .canonical_root
        .join(&custody.canonical.interface.interface_path);
    let graph_path = custody.graph.as_ref().unwrap().1.clone();
    let catalog_paths = fixture.catalog().modules[0].clone();
    let mutable = fixture.output.join(&catalog_paths.interface.path);
    fs::write(&mutable, b"changed deployment interface").unwrap();
    assert!(
        matches!(fixture.load(), Err(ModulePackageError::ArtifactChanged(path)) if path == mutable)
    );
    let continued = borrow_catalog(&package).unwrap();
    let continued_original = &continued.by_owner[&("u".into(), "Library".into())];
    assert_eq!(continued_original.iface_path, private_interface);
    assert_eq!(continued_original.product.bytes(), original.product.bytes());
    assert_eq!(fs::read(&private_interface).unwrap(), expected_interface);
    assert_eq!(counter.count(), 1, "borrowing neither reloads nor decodes");
    let entry = ArtifactEntry::canonical(original.original_module_interface.clone());
    let id = entry.descriptor.id;
    let inventory = ArtifactInventory::default();
    let parent = inventory
        .admit(&inventory.empty_view(), vec![entry])
        .unwrap();
    let detached = parent.select_roots(vec![id]).unwrap();
    assert!(!detached.original_input_origins(id).is_empty());
    drop(custody);
    drop(parent);
    drop(continued);
    drop(selected);
    drop(package);
    assert!(
        weak.upgrade().is_none(),
        "detached view retains exact custody, not the whole package"
    );
    assert!(private_interface.is_file());
    assert!(canonical_path.is_file());
    assert!(graph_path.is_file());
    drop(detached);
    assert!(
        !private_interface.exists(),
        "last certified original releases owned compiler paths"
    );
}

#[test]
fn acquired_catalog_refuses_protected_negative_search_and_package_observation_drift() {
    let external = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(external.path()).unwrap();
    let shadow = root.join("Library.hs");
    let package_interface = root.join("External.hi");
    let bytes = b"selected external package interface";
    fs::write(&package_interface, bytes).unwrap();
    let fixture = Fixture::with_modules_execution_packages_and_shadow(
        &["Library"],
        false,
        &std::collections::BTreeMap::from([(
            ("external-unit".into(), "External.Module".into()),
            crate::certified_products::PackageInterfaceWitness {
                selected_path: package_interface.clone(),
                sha256: super::super::super::parse_sha(&sha(bytes)).unwrap(),
            },
        )]),
        Some(&shadow),
    );
    let counter = DecodeCounter::new();
    let package = Arc::new(fixture.load().unwrap());
    assert!(!package.source_selection().contains_source(&shadow));
    borrow_catalog(&package).unwrap();
    fs::write(&shadow, b"module Library where\nvalue = 8\n").unwrap();
    assert!(borrow_catalog(&package).is_err());
    fs::remove_file(shadow).unwrap();
    borrow_catalog(&package).unwrap();
    fs::write(&package_interface, b"changed external package interface").unwrap();
    assert!(borrow_catalog(&package).is_err());
    assert!(fixture.load().is_err());
    fs::write(package_interface, bytes).unwrap();
    borrow_catalog(&package).unwrap();
    assert_eq!(counter.count(), 1);
}

#[test]
fn fresh_acquisition_authenticates_every_companion_while_borrowing_keeps_captured_bytes() {
    let fixture = Fixture::with_modules_and_execution(&["Library"], true);
    let package = Arc::new(fixture.load().unwrap());
    let catalog = fixture.catalog();
    let files = &catalog.modules[0];
    let paths = catalog
        .execution_graphs
        .iter()
        .map(|reference| &reference.path)
        .chain([
            &files.owner.path,
            &files.products.path,
            &files.interface.path,
            &files.packages.path,
            &files.evidence.path,
            &files.certification.path,
        ])
        .chain([
            &files.module_interface.interface.interface_path,
            &files.module_interface.interface.package_imports_path,
            &files.module_interface.certificate_path,
        ])
        .chain(files.module_interface.core.iter().map(|core| &core.path))
        .collect::<BTreeSet<_>>();
    for relative in paths {
        let artifact = fixture.output.join(relative);
        let original = fs::read(&artifact).unwrap();
        let mut changed = original.clone();
        changed.push(0);
        fs::write(&artifact, changed).unwrap();
        assert!(
            matches!(fixture.load(), Err(ModulePackageError::ArtifactChanged(path)) if path == artifact)
        );
        let continued = borrow_catalog(&package).unwrap();
        assert_eq!(
            continued.by_owner[&("u".into(), "Library".into())]
                .product
                .bytes(),
            package.records[0].products
        );
        fs::write(artifact, original).unwrap();
    }
    let mut wrong = fixture.authority.clone();
    wrong.consumed_worker_identity[0] ^= 1;
    assert!(matches!(
        DeploymentModulePackage::load_under(
            &fixture.output.join("catalog.json"),
            &wrong,
            RootPolicy::Fixture
        ),
        Err(ModulePackageError::CompilerMismatch)
    ));
}

#[test]
fn acquired_catalog_history_separates_source_drift_from_original_image_drift() {
    use proptest::test_runner::{Config, RngSeed, TestRunner};
    let mut config = Config::default();
    config.cases = 48;
    config.rng_seed = RngSeed::Fixed(2026101001);
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::acquired_catalog_history_separates_source_drift_from_original_image_drift"
    ));
    let configured_cases = config.cases;
    let partitions = std::cell::RefCell::new([0usize; 8]);
    let mut runner = TestRunner::new(config);
    let result = runner.run(&proptest::collection::vec(0u8..8, 1..16), |actions| {
        let fixture = Fixture::new();
        let package = Arc::new(fixture.load().unwrap());
        let before = package.records[0].products.clone();
        let reference = &fixture.catalog().modules[0];
        for action in actions {
            partitions.borrow_mut()[usize::from(action)] += 1;
            let path = match action {
                0 => fixture.source.join("lib/Library.hs"),
                1 => fixture.source.join("lib/Extra.hs"),
                2 => fixture.output.join("catalog.json"),
                3 => fixture.output.join(&reference.products.path),
                4 => fixture.output.join(&reference.evidence.path),
                5 => fixture
                    .output
                    .join(&reference.module_interface.certificate_path),
                6 => fixture
                    .output
                    .join(&reference.module_interface.core.as_ref().unwrap().path),
                _ => fixture.output.join(&reference.interface.path),
            };
            let original = fs::read(&path).ok();
            let mut changed = original.clone().unwrap_or_default();
            changed.extend_from_slice(b"\nchanged\n");
            fs::write(&path, changed).unwrap();
            proptest::prop_assert!(fixture.load().is_err());
            let borrowed = borrow_catalog(&package);
            if action <= 1 {
                proptest::prop_assert!(borrowed.is_err());
            } else {
                let borrowed = borrowed.unwrap();
                proptest::prop_assert_eq!(
                    borrowed.by_owner[&("u".into(), "Library".into())]
                        .product
                        .bytes(),
                    before.as_slice()
                );
            }
            match original {
                Some(original) => fs::write(path, original).unwrap(),
                None => fs::remove_file(path).unwrap(),
            }
            let restored = borrow_catalog(&package).unwrap();
            proptest::prop_assert_eq!(
                restored.by_owner[&("u".into(), "Library".into())]
                    .product
                    .bytes(),
                before.as_slice()
            );
        }
        Ok(())
    });
    eprintln!(
        "{}",
        serde_json::json!({"property":"acquired_catalog_history", "configured_cases":configured_cases,"action_partitions":*partitions.borrow()})
    );
    assert!(result.is_ok(), "{result:?}");
    assert!(partitions.borrow().iter().all(|count| *count > 0));
}

#[test]
fn cancelled_acquisition_preserves_explicit_owner_and_followup_can_acquire() {
    use tidepool_extract_cmd::{
        with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose,
    };
    let fixture = Fixture::new();
    let package = Arc::new(fixture.load().unwrap());
    let cancellation = CompilerTransactionCancellation::new();
    cancellation.cancel();
    let result = with_compiler_transaction_cancellable(cancellation, |_| {}, || fixture.load());
    assert!(matches!(
        result.action,
        Err(ModulePackageError::Interrupted(_))
    ));
    assert_eq!(result.close, CompilerTransactionClose::NotStarted);
    borrow_catalog(&package).unwrap();
    let fresh = fixture.load().unwrap();
    assert_eq!(fresh.catalog_identity(), package.catalog_identity());
}

#[test]
fn identical_canonical_admissions_preserve_new_and_distinct_catalog_custody() {
    use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory};
    for acquired_first in [false, true] {
        let fixture = Fixture::new();
        let package = Arc::new(fixture.load().unwrap());
        let selected = borrow_catalog(&package).unwrap();
        let canonical = selected.by_owner[&("u".into(), "Library".into())]
            .original_module_interface
            .clone();
        let bare = package.records[0]
            .module_interface_proof
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(canonical, bare);
        let original_path = selected.by_owner[&("u".into(), "Library".into())]
            .iface_path
            .clone();
        let inventory = ArtifactInventory::default();
        let first = ArtifactEntry::canonical(if acquired_first {
            canonical.clone()
        } else {
            bare.clone()
        });
        let id = first.descriptor.id;
        let view = inventory
            .admit(&inventory.empty_view(), vec![first])
            .unwrap();
        let second = ArtifactEntry::canonical(if acquired_first {
            bare
        } else {
            canonical.clone()
        });
        let retained = inventory.admit(&view, vec![second]).unwrap();
        drop(canonical);
        assert!(
            !retained.original_input_origins(id).is_empty(),
            "semantic equality cannot erase catalog provenance in either admission order"
        );
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::from_authenticated_interfaces(
                retained.descriptors()[0].producer_sha256,
                &retained,
            )
            .unwrap(),
        );
        let first_request = context
            .prepare_compilation(
                &fixture._root.path().join("first-scope"),
                &fixture.authority.producer_identity,
            )
            .unwrap();
        let moved = fixture._root.path().join("moved-products");
        fs::rename(&fixture.output, &moved).unwrap();
        let reacquired = Arc::new(
            DeploymentModulePackage::load_under(
                &moved.join("catalog.json"),
                &fixture.authority,
                RootPolicy::Fixture,
            )
            .unwrap(),
        );
        let new_selected = borrow_catalog(&reacquired).unwrap();
        let other = &new_selected.by_owner[&("u".into(), "Library".into())];
        let other_path = other.iface_path.clone();
        let both = inventory
            .admit(
                &retained,
                vec![ArtifactEntry::canonical(
                    other.original_module_interface.clone(),
                )],
            )
            .unwrap();
        let origins = both.original_input_origins(id);
        let paths = origins
            .iter()
            .flat_map(|origins| origins.origins())
            .map(|origin| origin.path.clone())
            .collect::<BTreeSet<_>>();
        assert!(paths.iter().any(|path| path.starts_with(&fixture.output)));
        assert!(paths.iter().any(|path| path.starts_with(&moved)));
        let current = Arc::new(
            (*context)
                .clone()
                .extend_interface_artifacts(&both)
                .unwrap(),
        );
        let current_request = current
            .prepare_compilation(
                &fixture._root.path().join("current-scope"),
                &fixture.authority.producer_identity,
            )
            .unwrap();
        assert_eq!(first_request.artifacts, current_request.artifacts);
        let image_rows = |path: &Path| {
            let scope: ciborium::value::Value =
                ciborium::de::from_reader(fs::read(path).unwrap().as_slice()).unwrap();
            scope.as_array().unwrap()[10].as_array().unwrap()[1]
                .as_array()
                .unwrap()
                .clone()
        };
        let previous_images = image_rows(&first_request.manifest);
        let current_images = image_rows(&current_request.manifest);
        assert_eq!(previous_images.len(), 1);
        assert_eq!(current_images.len(), 1);
        assert_eq!(
            previous_images[0].as_array().unwrap()[3],
            current_images[0].as_array().unwrap()[3],
            "strengthening origins does not replace immutable content identity"
        );
        let observed = current_images
            .iter()
            .flat_map(|image| image.as_array().unwrap()[4].as_array().unwrap())
            .flat_map(|part| part.as_array().unwrap()[4].as_array().unwrap())
            .map(|path| PathBuf::from(path.as_text().unwrap()))
            .collect::<BTreeSet<_>>();
        assert!(observed
            .iter()
            .any(|path| path.starts_with(&fixture.output)));
        assert!(
            observed.iter().any(|path| path.starts_with(&moved)),
            "fresh receiving image must observe current selected custody even when its aliases are inherited"
        );
        drop(current_request);
        drop(current);
        drop(first_request);
        drop(context);
        drop(new_selected);
        drop(reacquired);
        drop(selected);
        drop(package);
        drop(view);
        drop(retained);
        assert!(original_path.exists());
        assert!(other_path.exists());
        drop(both);
        assert!(!original_path.exists());
        assert!(!other_path.exists());
    }
}

#[test]
fn identical_native_and_same_batch_admissions_preserve_catalog_custody() {
    use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory};
    for native in [false, true] {
        for acquired_first in [false, true] {
            let fixture = Fixture::new();
            let package = Arc::new(fixture.load().unwrap());
            let selected = borrow_catalog(&package).unwrap();
            let canonical = selected.by_owner[&("u".into(), "Library".into())]
                .original_module_interface
                .clone();
            let bare = package.records[0]
                .module_interface_proof
                .as_ref()
                .unwrap()
                .clone();
            let make_entry = |interface: crate::certified_products::CertifiedModuleInterface| {
                if !native {
                    return ArtifactEntry::canonical(interface);
                }
                let record = &package.records[0];
                let mut candidate =
                    super::super::super::CandidateRecord::Deployment(Arc::clone(record));
                let product =
                    crate::certified_products::certify_candidate_original_with_validation(
                        crate::certified_products::OriginalNativeCandidate {
                            owner: record.original_owner.owner(),
                            product: candidate.product(Arc::clone(record.product())),
                            certification_bytes: record.original_certification.clone(),
                            module_interface: interface,
                            execution_source: record.execution_source.clone(),
                        },
                        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                    )
                    .unwrap();
                ArtifactEntry::original(canonical.producer_sha256(), product).unwrap()
            };
            let captured_entry = make_entry(canonical.clone());
            let id = captured_entry.descriptor.id;
            let bare_entry = make_entry(bare.clone());
            let bare_canonical = ArtifactEntry::canonical(bare);
            let canonical_id = bare_canonical.descriptor.id;
            let inventory = ArtifactInventory::default();
            let mut entries = if acquired_first {
                vec![captured_entry, bare_entry]
            } else {
                vec![bare_entry, captured_entry]
            };
            if native {
                entries.push(bare_canonical);
            }
            let retained = inventory.admit(&inventory.empty_view(), entries).unwrap();
            assert!(!retained.original_input_origins(id).is_empty());
            assert!(!retained.original_input_origins(canonical_id).is_empty());
            // Repeated acquisition of identical protected origins must retain
            // the existing captured materialization, without accumulating leases.
            let reacquired = Arc::new(fixture.load().unwrap());
            let new_selected = borrow_catalog(&reacquired).unwrap();
            let incoming = new_selected.by_owner[&("u".into(), "Library".into())]
                .original_module_interface
                .clone();
            let incoming_path = new_selected.by_owner[&("u".into(), "Library".into())]
                .iface_path
                .clone();
            let continued = inventory
                .admit(&retained, vec![make_entry(incoming)])
                .unwrap();
            drop(new_selected);
            drop(reacquired);
            assert!(!incoming_path.exists());
            let origin_facts = |view: &crate::artifact_inventory::ArtifactView| {
                view.original_input_origins(id)
                    .iter()
                    .flat_map(|origins| {
                        origins.origins().iter().map(|origin| {
                            (
                                origin.kind,
                                origin.path.clone(),
                                origin.sha256,
                                origin.bytes,
                            )
                        })
                    })
                    .collect::<BTreeSet<_>>()
            };
            assert_eq!(origin_facts(&continued), origin_facts(&retained));
        }
    }
}

fn origin_paths(
    view: &crate::artifact_inventory::ArtifactView,
    id: crate::artifact_inventory::ArtifactId,
) -> BTreeSet<PathBuf> {
    view.original_input_origins(id)
        .iter()
        .flat_map(|origins| origins.origins().iter().map(|origin| origin.path.clone()))
        .collect()
}

#[test]
fn independent_catalog_views_preserve_cold_and_warm_issuing_custody() {
    use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory};
    for warm in [false, true] {
        let fixture = Fixture::new();
        let package = Arc::new(fixture.load().unwrap());
        let selected = borrow_catalog(&package).unwrap();
        let interface = selected.by_owner[&("u".into(), "Library".into())]
            .original_module_interface
            .clone();
        let expected_a = interface
            .original_input_origins()
            .unwrap()
            .origins()
            .iter()
            .map(|origin| origin.path.clone())
            .collect::<BTreeSet<_>>();
        let first_path = selected.by_owner[&("u".into(), "Library".into())]
            .iface_path
            .clone();
        let inventory = ArtifactInventory::default();
        let first_entry = ArtifactEntry::canonical(interface);
        let id = first_entry.descriptor.id;
        let a = inventory
            .admit(&inventory.empty_view(), vec![first_entry])
            .unwrap();
        if warm {
            assert_eq!(origin_paths(&a, id), expected_a);
        }
        let moved = fixture._root.path().join("moved-products");
        fs::rename(&fixture.output, &moved).unwrap();
        let new_package = Arc::new(
            DeploymentModulePackage::load_under(
                &moved.join("catalog.json"),
                &fixture.authority,
                RootPolicy::Fixture,
            )
            .unwrap(),
        );
        let new_selected = borrow_catalog(&new_package).unwrap();
        let other = &new_selected.by_owner[&("u".into(), "Library".into())];
        let expected_b = other
            .original_module_interface
            .original_input_origins()
            .unwrap()
            .origins()
            .iter()
            .map(|origin| origin.path.clone())
            .collect::<BTreeSet<_>>();
        let second_path = other.iface_path.clone();
        let b = inventory
            .admit(
                &inventory.empty_view(),
                vec![ArtifactEntry::canonical(
                    other.original_module_interface.clone(),
                )],
            )
            .unwrap();
        assert_eq!(
            origin_paths(&a, id),
            expected_a,
            "unrelated admission cannot change a cold or warm view"
        );
        assert_eq!(
            origin_paths(&b, id),
            expected_b,
            "an independent view cannot inherit another issuer's origins"
        );
        let expected_join = expected_a
            .union(&expected_b)
            .cloned()
            .collect::<BTreeSet<_>>();
        let ab = a.merge(&b).unwrap();
        let ba = b.merge(&a).unwrap();
        assert_eq!(origin_paths(&ab, id), expected_join);
        assert_eq!(origin_paths(&ba, id), expected_join);
        let other_inventory = ArtifactInventory::default();
        let other_b = other_inventory
            .admit_shared(&other_inventory.empty_view(), b.entries())
            .unwrap();
        let cross_ab = a.merge(&other_b).unwrap();
        let cross_ba = other_b.merge(&a).unwrap();
        assert_eq!(origin_paths(&cross_ab, id), expected_join);
        assert_eq!(origin_paths(&cross_ba, id), expected_join);
        assert_eq!(origin_paths(&other_b, id), expected_b);
        let detached_b = b.select_roots(vec![id]).unwrap();
        assert_eq!(origin_paths(&detached_b, id), expected_b);
        drop(ab);
        drop(ba);
        drop(cross_ab);
        drop(cross_ba);
        drop(other_b);
        drop(a);
        drop(b);
        drop(selected);
        drop(package);
        drop(new_selected);
        drop(new_package);
        assert!(
            !first_path.exists(),
            "projected B does not retain unrelated A materialization"
        );
        assert!(second_path.exists());
        drop(detached_b);
        assert!(!second_path.exists());
    }
}

#[test]
fn dependency_only_catalog_closure_preserves_bytes_without_ambient_origin_custody() {
    use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory, ArtifactPayload};
    let fixture = Fixture::with_dependency();
    let package = Arc::new(fixture.load().unwrap());
    let selected = borrow_catalog(&package).unwrap();
    let inventory = ArtifactInventory::default();
    let first_a = ArtifactEntry::canonical(
        selected.by_owner[&("u".into(), "A".into())]
            .original_module_interface
            .clone(),
    );
    let id_a = first_a.descriptor.id;
    let first_b = ArtifactEntry::canonical(
        selected.by_owner[&("u".into(), "B".into())]
            .original_module_interface
            .clone(),
    );
    let id_b = first_b.descriptor.id;
    let old_path = selected.by_owner[&("u".into(), "A".into())]
        .iface_path
        .clone();
    let original_a = selected.by_owner[&("u".into(), "A".into())]
        .original_module_interface
        .interface_bytes()
        .to_vec();
    let old = inventory
        .admit(&inventory.empty_view(), vec![first_a, first_b])
        .unwrap();
    let moved = fixture._root.path().join("moved-products");
    fs::rename(&fixture.output, &moved).unwrap();
    let new_package = Arc::new(
        DeploymentModulePackage::load_under(
            &moved.join("catalog.json"),
            &fixture.authority,
            RootPolicy::Fixture,
        )
        .unwrap(),
    );
    let new_selected = borrow_catalog(&new_package).unwrap();
    let incoming = || {
        ArtifactEntry::canonical(
            new_selected.by_owner[&("u".into(), "B".into())]
                .original_module_interface
                .clone(),
        )
    };
    let b = inventory
        .admit(&inventory.empty_view(), vec![incoming()])
        .unwrap();
    assert!(
        origin_paths(&b, id_a).is_empty(),
        "hidden dependency bytes cannot grant unrelated physical issuer custody"
    );
    assert!(origin_paths(&b, id_b)
        .iter()
        .all(|path| path.starts_with(&moved)));
    let old_a = old.select_roots(vec![id_a]).unwrap();
    let issued_parent = inventory.admit(&old_a, vec![incoming()]).unwrap();
    assert_eq!(
        origin_paths(&issued_parent, id_a),
        origin_paths(&old_a, id_a)
    );
    assert_eq!(origin_paths(&issued_parent, id_b), origin_paths(&b, id_b));
    drop(issued_parent);
    drop(old_a);
    drop(old);
    drop(selected);
    drop(package);
    assert!(!old_path.exists());
    let semantic = b
        .entries()
        .into_iter()
        .find(|entry| entry.descriptor.id == id_a)
        .unwrap();
    let ArtifactPayload::Canonical(interface) = &semantic.payload else {
        panic!("canonical dependency");
    };
    assert_eq!(interface.interface_bytes(), original_a);
    assert!(!interface.certificate_bytes().is_empty());
    assert!(
        interface.core_bytes().is_some(),
        "shared semantic payload retains complete original inputs"
    );
}

#[test]
fn acquired_catalog_append_history_retains_only_new_or_changed_custody_handles() {
    use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory};
    let fixture = Fixture::with_modules(&["A", "B"]);
    let inventory = ArtifactInventory::default();
    let mut directory = fixture.output.clone();
    let mut view = inventory.empty_view();
    let mut expected_handles = 0;
    let mut census = Vec::new();
    for generation in 0..4 {
        if generation > 0 {
            let moved = fixture._root.path().join(format!("products-{generation}"));
            fs::rename(&directory, &moved).unwrap();
            directory = moved;
        }
        let package = Arc::new(
            DeploymentModulePackage::load_under(
                &directory.join("catalog.json"),
                &fixture.authority,
                RootPolicy::Fixture,
            )
            .unwrap(),
        );
        let selected = borrow_catalog(&package).unwrap();
        for repeat in 0..16 {
            let entries = selected
                .by_owner
                .values()
                .map(|original| {
                    ArtifactEntry::canonical(original.original_module_interface.clone())
                })
                .collect();
            view = inventory.admit(&view, entries).unwrap();
            if repeat == 0 {
                expected_handles += selected.by_owner.len();
            }
            assert_eq!(view.retained_catalog_custody_handles(), expected_handles,
                "unchanged offers share ancestor issuing handles; only changed custody adds an override");
            assert_eq!(view.entries().len(), 2);
        }
        census.push(view.retained_catalog_custody_handles());
    }
    assert_eq!(census, vec![2, 4, 6, 8]);
    eprintln!("acquired custody append census: 64 admissions, 4 actual acquisitions, retained handles {census:?}");
}
