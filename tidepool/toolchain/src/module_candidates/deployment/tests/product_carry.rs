use super::*;
use crate::module_candidates::product_decode_observer::DecodeCounter;
use tidepool_repr::execution_schema::RawModuleProduct;

#[test]
#[ignore = "requires a qualified frozen catalog and its exact compiler deployment environment"]
fn configured_package_validation_timing_workload() {
    let path = PathBuf::from(
        std::env::var_os("TIDEPOOL_PACKAGE_VALIDATION_BENCHMARK_CATALOG")
            .expect("select the matched frozen catalog explicitly"),
    );
    let crate::toolchain::CompilerDeploymentConfiguration::Configured(authority) =
        crate::toolchain::CompilerDeploymentConfiguration::from_env().unwrap()
    else {
        panic!("benchmark requires the matched configured compiler authority");
    };
    let mut owner = ConfiguredModulePackageOwner::new();
    let counter = DecodeCounter::new();
    let started = std::time::Instant::now();
    let package = owner.load(&path, &authority).unwrap();
    let cold_elapsed = started.elapsed();
    assert!(!package.records.is_empty());
    assert_eq!(counter.count(), package.records.len());
    let proof_owners = package
        .records
        .iter()
        .map(|record| std::ptr::from_ref(&*record.evidence) as usize)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    println!(
        "{}",
        serde_json::json!({
            "phase": "cold_configured_package", "elapsed_ns": cold_elapsed.as_nanos(),
            "catalog_sha256": package.catalog_identity(), "modules": package.records.len(),
            "native_decodes": counter.count(), "retained_proof_owners": proof_owners,
        })
    );
    for sample in 0..5 {
        let before = counter.count();
        let started = std::time::Instant::now();
        let work = package.revalidate(&path, RootPolicy::NixStore).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(counter.count(), before);
        println!(
            "{}",
            serde_json::json!({
                "phase": "warm_package_revalidation", "sample": sample,
                "elapsed_ns": elapsed.as_nanos(), "native_decodes": counter.count() - before,
                "artifact_read_files": work.artifact_files, "artifact_read_bytes": work.artifact_bytes,
                "evidence_source_read_attempts": work.evidence.source_read_attempts,
                "evidence_source_read_bytes": work.evidence.source_read_bytes,
                "evidence_negative_metadata_calls": work.evidence.negative_metadata_calls,
            })
        );
    }
}

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
    assert!(fixture.load().is_ok(), "fresh acquisition succeeds after restore");
    assert!(
        package
            .revalidate(&fixture.output.join("catalog.json"), RootPolicy::Fixture)
            .is_ok()
    );
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
            prop_assert!(
                package
                    .revalidate(&catalog_path, RootPolicy::Fixture)
                    .is_ok()
            );
        }
        Ok(())
    });
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn warm_shared_deployment_evidence_rechecks_negative_witnesses_outside_the_source_manifest() {
    let external = tempfile::tempdir().unwrap();
    let shadow = fs::canonicalize(external.path()).unwrap().join("A.hs");
    let fixture = Fixture::with_modules_execution_packages_and_shadow(
        &["A", "B"],
        false,
        &Default::default(),
        Some(&shadow),
    );
    let counter = DecodeCounter::new();
    let mut owner = ConfiguredModulePackageOwner::new();
    let path = fixture.output.join("catalog.json");
    let package = owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert!(!package.source_selection().contains_source(&shadow));
    assert!(std::ptr::eq(
        &*package.records[0].evidence,
        &*package.records[1].evidence
    ));
    let work = package.revalidate(&path, RootPolicy::Fixture).unwrap();
    assert_eq!(work.evidence.source_read_attempts, 2);
    assert_eq!(work.evidence.negative_metadata_calls, 1);
    fs::write(&shadow, b"module A where\nvalue = 8\n").unwrap();
    assert!(matches!(
        owner.load_under(&path, &fixture.authority, RootPolicy::Fixture),
        Err(ModulePackageError::OpenCohort)
    ));
    fs::remove_file(shadow).unwrap();
    let restored = owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert!(Arc::ptr_eq(&package, &restored));
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
fn configured_owner_reuses_decoded_products_across_fresh_offers_and_releases_replaced_selection() {
    let fixture = Fixture::with_modules(&["A", "B"]);
    let mut owner = ConfiguredModulePackageOwner::new();
    let counter = DecodeCounter::new();
    let path = fixture.output.join("catalog.json");
    let first = owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert_eq!(counter.count(), 2);
    assert!(matches!(
        first.candidates(&[9; 32]),
        Err(ModulePackageError::CompilerMismatch)
    ));
    for _ in 0..3 {
        let package = owner
            .load_under(&path, &fixture.authority, RootPolicy::Fixture)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &package));
        let scratch = tempfile::tempdir().unwrap();
        let selected = super::super::super::select_records_inner(
            &[3; 32],
            &package.source_selection().include_roots(),
            scratch.path(),
            package.candidates(&[3; 32]).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(selected.by_owner.len(), 2);
        for record in &package.records {
            let product = &selected.by_owner[&(record.unit.clone(), record.module.clone())].product;
            assert!(Arc::ptr_eq(&product.decoded, &record.product));
            let super::super::super::CandidateProductBytes::Deployment(carried) = &product.bytes
            else {
                panic!("retained deployment bytes");
            };
            assert!(Arc::ptr_eq(carried, record));
        }
        assert_eq!(
            counter.count(),
            2,
            "reuse and fresh offers perform no native decode"
        );
    }
    let weak = Arc::downgrade(&first);
    drop(first);
    let other = Fixture::new();
    owner
        .load_under(
            &other.output.join("catalog.json"),
            &other.authority,
            RootPolicy::Fixture,
        )
        .unwrap();
    assert_eq!(counter.count(), 3);
    assert!(
        weak.upgrade().is_none(),
        "owner retains only its current selection"
    );
    owner.clear();
    owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert_eq!(
        counter.count(),
        5,
        "a new owner lifetime authenticates and decodes anew"
    );
}

#[test]
fn configured_owner_does_not_publish_failed_initial_hydration() {
    let fixture = Fixture::with_modules(&["A", "B"]);
    let mut owner = ConfiguredModulePackageOwner::new();
    let counter = DecodeCounter::new();
    let path = fixture.output.join("catalog.json");
    let product = fixture
        .output
        .join(&fixture.catalog().modules[1].products.path);
    let original = fs::read(&product).unwrap();
    fs::write(&product, b"broken").unwrap();
    assert!(owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .is_err());
    assert!(owner.current.is_none());
    let after_failure = counter.count();
    fs::write(product, original).unwrap();
    owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert_eq!(
        counter.count(),
        after_failure + 2,
        "retry hydrates the complete cohort"
    );
}

#[test]
fn warm_configured_owner_refuses_external_package_interface_drift_and_restores_exact_custody() {
    let external = tempfile::tempdir().unwrap();
    let interface = fs::canonicalize(external.path())
        .unwrap()
        .join("External.hi");
    let original = b"selected external package interface";
    fs::write(&interface, original).unwrap();
    let key = ("external-unit".to_owned(), "External.Module".to_owned());
    let fixture = Fixture::with_modules_execution_and_packages(
        &["Library"],
        false,
        &std::collections::BTreeMap::from([(
            key.clone(),
            crate::certified_products::PackageInterfaceWitness {
                selected_path: interface.clone(),
                sha256: super::super::super::parse_sha(&sha(original)).unwrap(),
            },
        )]),
    );
    let mut owner = ConfiguredModulePackageOwner::new();
    let counter = DecodeCounter::new();
    let catalog = fixture.output.join("catalog.json");
    let package = owner
        .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert_eq!(counter.count(), 1);
    assert!(!interface.starts_with(&fixture.output));
    assert!(!package.source_selection().contains_source(&interface));
    let record = &package.records[0];
    let observed = crate::recovery_artifacts::validate_package_imports_with_validation(
        &record.package_imports,
        &record.unit,
        &record.module,
        &record.original_owner.skinny_iface_sha256,
        &fixture.output,
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[&key], (interface.clone(), sha(original)));
    let warm = owner
        .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert!(Arc::ptr_eq(&package, &warm));
    let catalog_identity = sha(&fs::read(&catalog).unwrap());

    fs::write(&interface, b"changed selected external package interface").unwrap();
    assert!(
        matches!(owner.load_under(&catalog, &fixture.authority, RootPolicy::Fixture), Err(ModulePackageError::ArtifactChanged(changed)) if changed == interface)
    );
    assert_eq!(sha(&fs::read(&catalog).unwrap()), catalog_identity);
    assert_eq!(
        counter.count(),
        1,
        "external drift refuses before native hydration"
    );
    fs::write(interface, original).unwrap();
    let restored = owner
        .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
    assert!(Arc::ptr_eq(&package, &restored));
    assert!(Arc::ptr_eq(&record.product, &restored.records[0].product));
    assert_eq!(counter.count(), 1);
}

#[test]
fn selected_candidates_retain_replaced_records_and_products_until_the_last_consumer_releases() {
    let first = Fixture::with_modules(&["A"]);
    let replacement = Fixture::with_modules(&["B"]);
    let mut owner = ConfiguredModulePackageOwner::new();
    let counter = DecodeCounter::new();
    let package = owner
        .load_under(
            &first.output.join("catalog.json"),
            &first.authority,
            RootPolicy::Fixture,
        )
        .unwrap();
    let weak_package = Arc::downgrade(&package);
    let weak_record = Arc::downgrade(&package.records[0]);
    let weak_product = Arc::downgrade(&package.records[0].product);
    let scratch = tempfile::tempdir().unwrap();
    let selected = Arc::new(
        super::super::super::select_records_inner(
            &[3; 32],
            &package.source_selection().include_roots(),
            scratch.path(),
            package.candidates(&[3; 32]).unwrap(),
            None,
        )
        .unwrap(),
    );
    assert_eq!(selected.by_owner.len(), 1);
    let key = ("u".to_owned(), "A".to_owned());
    assert!(Arc::ptr_eq(
        &selected.by_owner[&key].product.decoded,
        &package.records[0].product
    ));
    assert_eq!(counter.count(), 1);
    drop(package);
    owner
        .load_under(
            &replacement.output.join("catalog.json"),
            &replacement.authority,
            RootPolicy::Fixture,
        )
        .unwrap();
    assert_eq!(counter.count(), 2);
    assert!(
        weak_package.upgrade().is_none(),
        "the bounded slot releases the replaced package"
    );
    assert!(weak_record.upgrade().is_some());
    assert!(weak_product.upgrade().is_some());
    let last_consumer = Arc::clone(&selected);
    drop(selected);
    assert!(weak_record.upgrade().is_some());
    assert!(weak_product.upgrade().is_some());
    assert_eq!(
        last_consumer.by_owner[&key].product.bytes(),
        product_bytes("u", "A", b"A")
    );
    assert_eq!(last_consumer.by_owner[&key].product.module, "A");
    drop(last_consumer);
    assert!(
        weak_record.upgrade().is_none(),
        "the final selected consumer releases record custody"
    );
    assert!(
        weak_product.upgrade().is_none(),
        "the final selected consumer releases decoded product custody"
    );
}

#[test]
fn synchronized_configured_owner_coalesces_lookups_and_preserves_replacement_and_refusal_selections(
) {
    use crate::toolchain::with_configured_module_package_owner;
    use std::sync::{mpsc, Mutex};
    let first = Fixture::with_modules(&["A"]);
    let replacement = Fixture::with_modules(&["B"]);
    let owner = Arc::new(Mutex::new(ConfiguredModulePackageOwner::new()));
    let first_path = first.output.join("catalog.json");
    let replacement_path = replacement.output.join("catalog.json");
    let first_identity = sha(&fs::read(&first_path).unwrap());
    let replacement_identity = sha(&fs::read(&replacement_path).unwrap());
    assert_ne!(first_identity, replacement_identity);
    let (ready, admitted) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let guard = owner.lock().unwrap();
    let cold = std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|_| {
                let owner = Arc::clone(&owner);
                let ready = ready.clone();
                let done = done.clone();
                let path = &first_path;
                let authority = &first.authority;
                scope.spawn(move || {
                    let counter = DecodeCounter::new();
                    ready.send(()).unwrap();
                    let package = with_configured_module_package_owner(&owner, |owner| {
                        owner.load_under(path, authority, RootPolicy::Fixture)
                    })
                    .unwrap();
                    done.send(()).unwrap();
                    (package, counter.count())
                })
            })
            .collect::<Vec<_>>();
        for _ in 0..4 {
            admitted.recv().unwrap();
        }
        assert!(matches!(
            completed.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(guard);
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        cold.iter().map(|(_, count)| *count).sum::<usize>(),
        1,
        "one cold hydration serves concurrent callers"
    );
    let original = &cold[0].0;
    for (package, _) in &cold {
        assert!(Arc::ptr_eq(original, package));
        assert_eq!(package.catalog_identity(), first_identity);
        assert_eq!(package.source_selection().snapshot_root, first.source);
    }

    let (entered, holding) = mpsc::channel();
    let (release, continue_warm) = mpsc::channel();
    let (ready, admitted) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let mut wrong = first.authority.clone();
    wrong.consumed_worker_identity[0] ^= 1;
    let (warm, replacements) =
        std::thread::scope(|scope| {
            // Disconnect the gate if the parent assertion fails, so scoped
            // children cannot strand the test while it unwinds.
            let release = release;
            let warm_owner = Arc::clone(&owner);
            let warm_path = &first_path;
            let authority = &first.authority;
            let warm = scope.spawn(move || {
                let counter = DecodeCounter::new();
                let package = with_configured_module_package_owner(&warm_owner, |owner| {
                    let package = owner
                        .load_under(warm_path, authority, RootPolicy::Fixture)
                        .unwrap();
                    entered.send(()).unwrap();
                    continue_warm.recv().unwrap();
                    Ok(package)
                })
                .unwrap();
                (package, counter.count())
            });
            holding.recv().unwrap();
            let replacements = (0..2)
                .map(|_| {
                    let owner = Arc::clone(&owner);
                    let ready = ready.clone();
                    let done = done.clone();
                    let path = &replacement_path;
                    let authority = &replacement.authority;
                    scope.spawn(move || {
                        let counter = DecodeCounter::new();
                        ready.send(()).unwrap();
                        let package = with_configured_module_package_owner(&owner, |owner| {
                            owner.load_under(path, authority, RootPolicy::Fixture)
                        })
                        .unwrap();
                        done.send(()).unwrap();
                        (package, counter.count())
                    })
                })
                .collect::<Vec<_>>();
            let refused_owner = Arc::clone(&owner);
            let refused_path = &first_path;
            let refused =
                scope.spawn(move || {
                    let counter = DecodeCounter::new();
                    ready.send(()).unwrap();
                    assert!(matches!(
                        with_configured_module_package_owner(&refused_owner, |owner| owner
                            .load_under(refused_path, &wrong, RootPolicy::Fixture)),
                        Err(ModulePackageError::CompilerMismatch)
                    ));
                    done.send(()).unwrap();
                    assert_eq!(counter.count(), 0);
                });
            for _ in 0..3 {
                admitted.recv().unwrap();
            }
            assert!(matches!(
                completed.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            release.send(()).unwrap();
            let warm = warm.join().unwrap();
            let replacements = replacements
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>();
            refused.join().unwrap();
            (warm, replacements)
        });
    assert!(Arc::ptr_eq(original, &warm.0));
    assert_eq!(warm.1, 0);
    assert_eq!(
        replacements.iter().map(|(_, count)| *count).sum::<usize>(),
        1
    );
    for (package, _) in &replacements {
        assert!(Arc::ptr_eq(&replacements[0].0, package));
        assert_eq!(package.catalog_identity(), replacement_identity);
        assert_eq!(package.source_selection().snapshot_root, replacement.source);
    }
    let current = with_configured_module_package_owner(&owner, |owner| {
        owner.load_under(
            &replacement_path,
            &replacement.authority,
            RootPolicy::Fixture,
        )
    })
    .unwrap();
    assert!(
        Arc::ptr_eq(&current, &replacements[0].0),
        "refusal cannot replace the current selection"
    );
}

#[test]
fn configured_owner_reauthenticates_every_package_companion_and_exact_authority_selection() {
    let fixture = Fixture::with_modules_and_execution(&["Library"], true);
    let mut owner = ConfiguredModulePackageOwner::new();
    let counter = DecodeCounter::new();
    let path = fixture.output.join("catalog.json");
    let package = owner
        .load_under(&path, &fixture.authority, RootPolicy::Fixture)
        .unwrap();
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
            matches!(owner.load_under(&path, &fixture.authority, RootPolicy::Fixture), Err(ModulePackageError::ArtifactChanged(changed)) if changed == artifact)
        );
        fs::write(artifact, original).unwrap();
        let restored = owner
            .load_under(&path, &fixture.authority, RootPolicy::Fixture)
            .unwrap();
        assert!(Arc::ptr_eq(&package, &restored));
        assert_eq!(counter.count(), 1);
    }
    for field in 0..3 {
        let mut authority = fixture.authority.clone();
        match field {
            0 => authority.frontend_path = PathBuf::from("/different/configured/frontend"),
            1 => authority.worker_path = PathBuf::from("/different/configured/worker"),
            _ => authority.ghc_libdir = PathBuf::from("/different/configured/ghc"),
        }
        let replacement = owner
            .load_under(&path, &authority, RootPolicy::Fixture)
            .unwrap();
        assert!(!Arc::ptr_eq(&package, &replacement));
        assert_eq!(
            counter.count(),
            2 + field * 2,
            "the complete authority selection defines the lifetime"
        );
        owner
            .load_under(&path, &fixture.authority, RootPolicy::Fixture)
            .unwrap();
        assert_eq!(counter.count(), 3 + field * 2);
    }
}

#[test]
fn configured_owner_history_refuses_drift_without_redecoding() {
    use proptest::test_runner::{Config, FileFailurePersistence, RngSeed, TestRunner};
    let mut config = Config::default();
    config.cases = 32;
    config.rng_seed = RngSeed::Fixed(2026100905);
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::configured_owner_history_refuses_drift_without_redecoding"
    ));
    let configured_cases = config.cases;
    let callbacks = std::cell::Cell::new(0usize);
    let completed = std::cell::Cell::new(0usize);
    let partitions = std::cell::RefCell::new([0usize; 12]);
    let mut runner = TestRunner::new(config);
    let result = runner.run(&proptest::collection::vec(0u8..12, 1..12), |actions| {
        callbacks.set(callbacks.get() + 1);
        let fixture = Fixture::new();
        let mut owner = ConfiguredModulePackageOwner::new();
        let counter = DecodeCounter::new();
        let catalog = fixture.output.join("catalog.json");
        let package = owner
            .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
            .unwrap();
        for action in actions {
            partitions.borrow_mut()[usize::from(action)] += 1;
            let reference = &fixture.catalog().modules[0];
            match action {
                0 => {
                    let mut wrong = fixture.authority.clone();
                    wrong.consumed_worker_identity[0] ^= 1;
                    proptest::prop_assert!(matches!(
                        owner.load_under(&catalog, &wrong, RootPolicy::Fixture),
                        Err(ModulePackageError::CompilerMismatch)
                    ));
                }
                1 => {
                    let source = fixture.source.join("lib/Library.hs");
                    let original = fs::read(&source).unwrap();
                    fs::write(&source, b"changed source").unwrap();
                    proptest::prop_assert!(owner
                        .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
                        .is_err());
                    fs::write(source, original).unwrap();
                }
                2..=8 => {
                    let path = match action {
                        2 => catalog.clone(),
                        3 => fixture.output.join(&reference.products.path),
                        4 => fixture.output.join(&reference.evidence.path),
                        5 => fixture.output.join(&reference.certification.path),
                        6 => fixture
                            .output
                            .join(&reference.module_interface.certificate_path),
                        7 => fixture
                            .output
                            .join(&reference.module_interface.interface.package_imports_path),
                        _ => fixture
                            .output
                            .join(&reference.module_interface.core.as_ref().unwrap().path),
                    };
                    let original = fs::read(&path).unwrap();
                    let mut changed = original.clone();
                    changed.push(0);
                    fs::write(&path, changed).unwrap();
                    proptest::prop_assert!(matches!(
                        owner.load_under(&catalog, &fixture.authority, RootPolicy::Fixture),
                        Err(ModulePackageError::ArtifactChanged(_))
                    ));
                    fs::write(path, original).unwrap();
                }
                9 => {
                    let addition = fixture.source.join("lib/Extra.hs");
                    fs::write(&addition, b"module Extra where\n").unwrap();
                    proptest::prop_assert!(owner
                        .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
                        .is_err());
                    fs::remove_file(addition).unwrap();
                }
                10 => {
                    let source = fixture.source.join("lib/Library.hs");
                    let retained = fixture.source.join("Library.held");
                    fs::rename(&source, &retained).unwrap();
                    std::os::unix::fs::symlink(&retained, &source).unwrap();
                    proptest::prop_assert!(owner
                        .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
                        .is_err());
                    fs::remove_file(&source).unwrap();
                    fs::rename(retained, source).unwrap();
                }
                _ => {}
            }
            let restored = owner
                .load_under(&catalog, &fixture.authority, RootPolicy::Fixture)
                .unwrap();
            proptest::prop_assert!(Arc::ptr_eq(&package, &restored));
            proptest::prop_assert_eq!(counter.count(), 1);
        }
        completed.set(completed.get() + 1);
        Ok(())
    });
    eprintln!(
        "{}",
        serde_json::json!({"property": "configured_owner_history", "configured_cases": configured_cases, "callbacks": callbacks.get(), "completed_callbacks": completed.get(), "action_partitions": *partitions.borrow()})
    );
    assert!(result.is_ok(), "{result:?}");
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

#[test]
fn cancelled_configured_owner_waiter_preserves_current_and_followup_recovers() {
    use crate::toolchain::with_configured_module_package_owner;
    use tidepool_extract_cmd::{
        with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose,
    };
    let fixture = Fixture::with_modules(&["A"]);
    let replacement = Fixture::with_modules(&["B"]);
    let owner = std::sync::Mutex::new(ConfiguredModulePackageOwner::new());
    let path = fixture.output.join("catalog.json");
    let current = with_configured_module_package_owner(&owner, |owner| {
        owner.load_under(&path, &fixture.authority, RootPolicy::Fixture)
    })
    .unwrap();
    let mut held = owner.lock().unwrap();
    let cancellation = CompilerTransactionCancellation::new();
    let (entered, observed) = std::sync::mpsc::channel();
    let (finished, received) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let worker_cancellation = cancellation.clone();
        let owner = &owner;
        let replacement = &replacement;
        scope.spawn(move || {
            entered.send(()).unwrap();
            let result = with_compiler_transaction_cancellable(
                worker_cancellation,
                |_| {},
                || {
                    with_configured_module_package_owner(owner, |owner| {
                        owner.load_under(
                            &replacement.output.join("catalog.json"),
                            &replacement.authority,
                            RootPolicy::Fixture,
                        )
                    })
                },
            );
            finished.send(result).unwrap();
        });
        observed
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        cancellation.cancel();
        let result = received
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(
            matches!(result.action, Err(ModulePackageError::Interrupted(error)) if error.kind() == std::io::ErrorKind::Interrupted)
        );
        assert_eq!(result.close, CompilerTransactionClose::NotStarted);
        let retained = held
            .load_under(&path, &fixture.authority, RootPolicy::Fixture)
            .unwrap();
        assert!(Arc::ptr_eq(&current, &retained));
        drop(held);
    });
    let followup = with_configured_module_package_owner(&owner, |owner| {
        owner.load_under(
            &replacement.output.join("catalog.json"),
            &replacement.authority,
            RootPolicy::Fixture,
        )
    })
    .unwrap();
    assert!(!Arc::ptr_eq(&current, &followup));
    assert!(current
        .records
        .iter()
        .any(|record| record.record().module == "A"));
    assert!(followup
        .records
        .iter()
        .any(|record| record.record().module == "B"));
}
