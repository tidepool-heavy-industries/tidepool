use super::*;
use crate::module_candidates::product_decode_observer::DecodeCounter;
use tidepool_repr::execution_schema::RawModuleProduct;

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
                    package
                });
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
