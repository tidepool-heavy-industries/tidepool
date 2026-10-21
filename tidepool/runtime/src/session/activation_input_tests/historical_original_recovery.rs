//! Genuine World publication and cold historical Original recovery.
//! Explicit result bindings keep this control independent of expression-name issuance.
use super::*;
use crate::session::{
    CertifiedDeclarationPublication, ExecutionPublication, PublicManifestCommit,
    PublicationDecision, RecoveryPublicOwner,
};
use std::path::PathBuf;
use tidepool_codegen::prepared_program::ImageRegistry;
use tidepool_toolchain::certified_products::SelectedNativeImport;
use tidepool_toolchain::checked_cell::CheckedItemKind;

pub(super) fn run() {
    let subscriber = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "off,tidepool::artifact_inventory::ambiguity_trace=debug",
        ))
        .finish();
    tracing::subscriber::with_default(subscriber, run_history);
}

fn run_history() {
    tidepool_testing::eval_harness::require_extract();
    let diagnostic_root = PathBuf::from(
        std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
            .expect("cold producer-removal control requires the native runner's owned case root"),
    );
    let transactions = diagnostic_root.join("compiler-transactions");
    let transaction_directories = || match std::fs::read_dir(&transactions) {
        Ok(entries) => entries
            .map(|entry| {
                let entry = entry.unwrap();
                assert!(entry.file_type().unwrap().is_dir());
                entry.path()
            })
            .collect::<std::collections::BTreeSet<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => panic!("cannot inspect case compiler outputs: {error}"),
    };
    let before_outputs = transaction_directories();
    let mut root = tempfile::Builder::new()
        .prefix("historical-original-run-")
        .tempdir_in(&diagnostic_root)
        .unwrap();
    root.disable_cleanup(true);
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    std::fs::write(
        root.path().join("HistoricalOriginalG.hs"),
        include_str!("../fixtures/historical-original-g.hs"),
    )
    .unwrap();
    let source = root.path().join("HistoricalOriginalH.hs");
    let original_source = include_str!("../fixtures/historical-original-h.hs");
    std::fs::write(&source, original_source).unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(
        SessionId(1018),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public_source = persistent.mint_notebook_scope_with_lease(None).unwrap();
    let public = public_source.scope();
    let mut runtime =
        RuntimeTestSession::from_persistent_for_test(frunk::HNil, EmptyOutput, persistent);
    runtime
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let mut resident = TestSession {
        resident: runtime,
        public_source,
    };
    let mut include = effects.include_paths().to_vec();
    include.push(root.path().to_path_buf());
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        include.clone(),
    );
    let producer_owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/historical-original/producer").unwrap(),
        1,
    )
    .unwrap();
    assert!(matches!(
        resident
            .initialize_durable_public_scope(producer_owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident.bootstrap_world(&recipe);
    let publication = producer_owner;
    execute_historical_cell(
        &mut resident,
        &recipe,
        &publication,
        "import qualified HistoricalOriginalG\nlet historicalWarm = HistoricalOriginalG.g",
        None,
        |_| {},
    );
    let mut historical = None;
    execute_historical_cell(
        &mut resident,
        &recipe,
        &publication,
        include_str!("../fixtures/historical-original-world-reader.hs"),
        Some(("historicalReaderResult", 41)),
        |program| {
            let products = program.items().last().unwrap().native_products().unwrap();
            let selected = products.native_source_products().unwrap();
            let reader = selected
                .groups()
                .find(|row| {
                    row.group()
                        .group()
                        .binders()
                        .iter()
                        .any(|binder| binder.occurrence == "historicalRead")
                })
                .expect("the actual declaration selects its reader group");
            let child = selected
                .groups()
                .find(|row| row.group().owner().module == "HistoricalOriginalG")
                .expect("the reader selects its actual original child");
            assert!(reader.imports().iter().any(|import| {
                matches!(import, SelectedNativeImport::Source { binding, .. }
                    if *binding == child.binding())
            }));
            let implementation = selected
                .groups()
                .find(|row| row.group().owner().module == "HistoricalOriginalH")
                .expect("the original child retains its actual implementation dependency");
            assert!(child.imports().iter().any(|import| {
                matches!(import, SelectedNativeImport::Source { binding, .. }
                    if *binding == implementation.binding())
            }));
            historical = Some((
                reader.binding(),
                reader.group().owner().clone(),
                child.binding(),
                child.group().owner().clone(),
                implementation.binding(),
                implementation.group().owner().clone(),
            ));
        },
    );
    let (reader_binding, reader_owner, old_binding, old_owner, old_impl_binding, old_impl_owner) =
        historical.unwrap();
    let view = resident.compile_view_in(public).unwrap();
    let context = view.exact_declaration_context().unwrap();
    let descriptors = context.artifact_view().descriptors();
    let child_role = context
        .compiler_input_roles()
        .into_iter()
        .find(|role| {
            descriptors.iter().any(|descriptor| {
                descriptor.id == role.interface()
                    && descriptor.owner.module == "HistoricalOriginalG"
            })
        })
        .expect("the published reader retains its child's canonical interface");
    assert!(
        child_role.original().is_none(),
        "declaration publication must leave only the parent as reusable source"
    );
    let old_product = context
        .recovery_products()
        .into_iter()
        .find(|product| product.owner() == &old_owner)
        .expect("the parent retains its exact original child's immutable bytes");
    let old_interface_bytes = old_product.interface_bytes().to_vec();
    let selected_originals = context
        .compiler_input_roles()
        .into_iter()
        .filter_map(|role| role.original())
        .map(|original| {
            descriptors
                .iter()
                .find(|descriptor| descriptor.id == original)
                .unwrap()
                .owner
                .module
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_originals,
        [reader_owner.module.clone()],
        "the capture must select only R as Original while retaining G/H as dependency custody"
    );
    let cold_owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/historical-original/capture").unwrap(),
        1,
    )
    .unwrap();
    let captured = resident.mint_detached_scope(public).unwrap();
    let captured_lease = resident
        .state
        .prepared_lexical_scope_lease(captured)
        .unwrap();
    assert!(matches!(
        resident
            .initialize_durable_public_scope(cold_owner.clone(), captured)
            .unwrap(),
        PublicManifestCommit::Durable
    ));

    let current_source = original_source.replace("h = 41", "h = 99");
    assert_ne!(current_source, original_source);
    std::fs::write(&source, current_source).unwrap();
    let current = tidepool_testing::with_settlement(|settlement| {
        crate::compile_runtime_targets(
            &tidepool_toolchain::artifacts::CompileInvocation {
                source: include_str!("../fixtures/historical-original-probe.hs"),
                targets: &["__prepared"],
                include: &include,
                fallback_module_name: "HistoricalOriginalProbe",
            },
            |_, _, _| {},
            settlement,
        )
    })
    .unwrap();
    let current_products = current
        .artifacts()
        .native_source_products("__prepared")
        .unwrap()
        .unwrap();
    let current_child = current_products
        .groups()
        .find(|row| row.group().owner().module == "HistoricalOriginalG")
        .unwrap();
    let current_owner = current_child.group().owner();
    let current_product = current
        .artifacts()
        .recovery_products
        .iter()
        .find(|product| product.owner() == current_owner)
        .unwrap();
    assert_eq!(
        old_interface_bytes.as_slice(),
        current_product.interface_bytes(),
        "old and current implementation must actually share the complete interface bytes"
    );
    assert_eq!(
        old_owner.skinny_iface_sha256,
        current_owner.skinny_iface_sha256
    );
    let current_implementation = current_products
        .groups()
        .find(|row| row.group().owner().module == "HistoricalOriginalH")
        .unwrap();
    let current_impl_owner = current_implementation.group().owner();
    assert!(current_child.imports().iter().any(|import| {
        matches!(import, SelectedNativeImport::Source { binding, .. }
            if *binding == current_implementation.binding())
    }));
    assert_ne!(
        old_impl_owner.product_sha256,
        current_impl_owner.product_sha256
    );
    assert_ne!(old_impl_binding, current_implementation.binding());
    assert_ne!(old_binding, current_child.binding());
    eprintln!(
        "historical-original G_payload_equal={} old_G={old_binding:?} current_G={:?} old_H={old_impl_binding:?} current_H={:?}",
        old_owner.product_sha256 == current_owner.product_sha256,
        current_child.binding(),
        current_implementation.binding(),
    );
    assert_eq!(
        current
            .run("__prepared", 1024 * 1024, &mut frunk::HNil, &(), |_| {})
            .unwrap()
            .to_json(),
        serde_json::json!(99)
    );
    let current_impl_sha256 = current_impl_owner.product_sha256;
    let observe_original_history = |program: &tidepool_toolchain::checked_cell::CellProgram| {
        let mut selected_old = false;
        let mut selected_old_child = false;
        let mut selected_old_implementation = false;
        let mut selected_current = false;
        for item in program.items() {
            let products = item.native_products().unwrap();
            let selected = products.native_source_products().unwrap();
            for row in selected.groups() {
                if row.binding() == reader_binding {
                    selected_old = true;
                    assert!(row.imports().iter().any(|import| {
                        matches!(import, SelectedNativeImport::Source { binding, .. }
                            if *binding == old_binding)
                    }));
                }
                if row.binding() == old_binding {
                    selected_old_child = true;
                    assert!(row.imports().iter().any(|import| {
                        matches!(import, SelectedNativeImport::Source { binding, .. }
                            if *binding == old_impl_binding)
                    }));
                }
                if row.binding() == old_impl_binding {
                    selected_old_implementation = true;
                }
                if row.group().owner().module == "HistoricalOriginalG"
                    && row.binding() != old_binding
                {
                    let current_import = row.imports().iter().find_map(|import| {
                        let SelectedNativeImport::Source { binding, .. } = import else {
                            return None;
                        };
                        selected.groups().find(|child| {
                            child.binding() == *binding
                                && child.group().owner().module == "HistoricalOriginalH"
                        })
                    });
                    if let Some(implementation) = current_import {
                        assert_eq!(
                            implementation.group().owner().product_sha256,
                            current_impl_sha256,
                        );
                        assert_ne!(implementation.binding(), old_impl_binding);
                        selected_current = true;
                    }
                }
            }
        }
        assert!(
            selected_old,
            "the actual World stage must select the original parent"
        );
        assert!(
            selected_old_child && selected_old_implementation,
            "the actual World stage must retain both exact historical dependency rows"
        );
        assert!(
            selected_current,
            "the actual World stage must select the current child independently"
        );
    };
    // A new captured source snapshot observes H=99 while the published reader
    // retains its issued historical G/H dependency bindings.
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        include.clone(),
    );
    resident.bootstrap_world(&recipe);
    execute_historical_cell(
        &mut resident,
        &recipe,
        &publication,
        include_str!("../fixtures/historical-original-world-observe.hs"),
        Some(("historicalResult", 41099)),
        &observe_original_history,
    );

    let evidence = diagnostic_root.join("historical-original-producer-evidence");
    std::fs::create_dir(&evidence).unwrap();
    let mut disposable_outputs = Vec::new();
    for output in transaction_directories().difference(&before_outputs) {
        let transaction_path = output.join("transaction.json");
        let transaction = match std::fs::read(&transaction_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot inspect completed compiler transaction: {error}"),
        };
        let record: serde_json::Value = serde_json::from_slice(&transaction).unwrap();
        assert_eq!(record["phase"], "compiler_completed");
        assert_eq!(record["compiler_process_success"], true);
        assert_eq!(
            PathBuf::from(record["original_directory"].as_str().unwrap()),
            *output
        );
        let retained = evidence.join(output.file_name().unwrap());
        std::fs::create_dir(&retained).unwrap();
        std::fs::write(retained.join("transaction.json"), transaction).unwrap();
        for filename in [
            "consumed-sources.json",
            "compiler-request.bin",
            "compiler-cwd.bin",
            "compiler.stderr",
        ] {
            let source = output.join(filename);
            if source.try_exists().unwrap() {
                std::fs::copy(&source, retained.join(filename)).unwrap();
            }
        }
        let consumed = output.join("consumed-sources");
        if consumed.try_exists().unwrap() {
            let destination = retained.join("consumed-sources");
            std::fs::create_dir(&destination).unwrap();
            for source in std::fs::read_dir(consumed).unwrap() {
                let source = source.unwrap();
                assert!(source.file_type().unwrap().is_file());
                std::fs::copy(source.path(), destination.join(source.file_name())).unwrap();
            }
        }
        disposable_outputs.push(output.clone());
    }
    assert!(
        !disposable_outputs.is_empty(),
        "the absence control must observe actual compiler outputs"
    );
    std::fs::write(
        evidence.join("removed-output-inventory.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "original_directories": &disposable_outputs,
            "retained_run": root.path(),
            "reader_binding": reader_binding,
            "old_g_binding": old_binding,
            "old_h_binding": old_impl_binding,
        }))
        .unwrap(),
    )
    .unwrap();

    drop(current_products);
    drop(current);
    drop(old_product);
    drop(view);
    drop(old_interface_bytes);
    drop(resident);
    drop(images);
    drop(publication);
    drop(captured_lease);
    drop(recovery);

    for output in &disposable_outputs {
        if output.try_exists().unwrap() {
            std::fs::remove_dir_all(output).unwrap();
        }
        assert!(!output.try_exists().unwrap());
    }
    assert!(root.path().join("artifacts").is_dir());
    eprintln!(
        "historical-original cold run={} removed_outputs={} retained_diagnostics={}",
        root.path().display(),
        disposable_outputs.len(),
        evidence.display(),
    );

    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::reopen(
        crate::session::recovery_test_fixture::run_owner(root.path()),
    );
    let mut lib = SessionLib::open(
        SessionId(1019),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.recover_public_scope(&cold_owner).unwrap();
    let public_source = persistent.adopt_initial_notebook_scope(public).unwrap();
    let mut runtime =
        RuntimeTestSession::from_persistent_for_test(frunk::HNil, EmptyOutput, persistent);
    runtime
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let mut resident = TestSession {
        resident: runtime,
        public_source,
    };
    let publication = cold_owner.clone();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        include,
    );
    resident.bootstrap_world(&recipe);
    let recovered_view = resident.compile_view_in(public).unwrap();
    let recovered = recovered_view.exact_declaration_context().unwrap();
    let descriptors = recovered.artifact_view().descriptors();
    let selected_originals = recovered
        .compiler_input_roles()
        .into_iter()
        .filter_map(|role| role.original())
        .map(|original| {
            descriptors
                .iter()
                .find(|descriptor| descriptor.id == original)
                .unwrap()
                .owner
                .module
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_originals,
        [reader_owner.module],
        "cold acquisition must preserve the parent-only Original selection"
    );
    execute_historical_cell(
        &mut resident,
        &recipe,
        &publication,
        include_str!("../fixtures/historical-original-world-observe.hs"),
        Some(("historicalResult", 41099)),
        &observe_original_history,
    );
}

fn execute_historical_cell(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    owner: &RecoveryPublicOwner,
    source: &str,
    expected_result: Option<(&str, i64)>,
    observe: impl FnOnce(&tidepool_toolchain::checked_cell::CellProgram),
) {
    let public_context = resident.public_context();
    let execution = begin_fixture_execution(resident);
    let private = execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: private,
            ..public_context
        })
        .unwrap();
    let checked = check_fixture_cell(resident, recipe, source, execution.clone(), 0);
    observe(checked.prefix.cell_program());
    let mut expected_result_seen = false;
    for index in 0..checked.checked.items.len() {
        let item = checked.checked.checked_item(index).unwrap();
        if item.kind() == CheckedItemKind::Declaration {
            source_action(resident, |resident, _| {
                let reservation = resident
                    .admit_checked_item(checked.prefix.clone(), item)
                    .unwrap();
                resident.adopt_checked_declaration(reservation).unwrap();
            });
        } else {
            let (bound, compiled, reservation) = checked.compile_binding(resident, index);
            let outcome = source_action(resident, |resident, settlement| {
                if item.kind() == CheckedItemKind::Bind {
                    match bound.as_slice() {
                        [] => resident.run_with_sites(
                            "historical-original",
                            compiled.code(),
                            settlement,
                        ),
                        [binder] => resident.run_bind_with_sites(
                            &binder.name,
                            compiled.code(),
                            binder,
                            reservation.generation(),
                            settlement,
                        ),
                        _ => resident.run_projected_bind_with_sites(
                            "historical-original",
                            compiled.code(),
                            &bound,
                            reservation.generation(),
                            settlement,
                        ),
                    }
                } else {
                    assert_eq!(bound.len(), 1);
                    resident.run_observation_with_sites(
                        compiled.code(),
                        &bound[0],
                        reservation.generation(),
                        false,
                        settlement,
                    )
                }
            })
            .unwrap();
            assert!(
                matches!(outcome, ResidentOutcome::Completed { .. }),
                "historical cell completes: {outcome:?}"
            );
            if let Some((name, expected)) = expected_result {
                if item.binders() == [name] {
                    assert_eq!(bound.len(), 1);
                    assert_eq!(
                        bound[0].name, name,
                        "the exact checked result binder executes"
                    );
                    let ResidentOutcome::Completed { result, .. } = outcome else {
                        unreachable!()
                    };
                    assert_eq!(result.to_json(), serde_json::json!(expected));
                    expected_result_seen = true;
                }
            }
        }
    }
    assert!(
        expected_result.is_none() || expected_result_seen,
        "the authored result binding produces the expected historical value"
    );
    let intent = resident.freeze_private_execution(&execution).unwrap();
    source_action(resident, |resident, settlement| {
        let publication = resident
            .restage_execution_publication(owner.clone(), intent)
            .unwrap();
        let ticket = match publication {
            ExecutionPublication::Bindings(base) => base.stage().unwrap(),
            ExecutionPublication::Declarations(base) => {
                let CertifiedDeclarationPublication::Accepted(accepted) =
                    base.certify(settlement).unwrap()
                else {
                    panic!("historical declaration publication must be accepted")
                };
                accepted.stage().unwrap()
            }
        };
        assert!(matches!(
            resident
                .attempt_public_manifest(ticket, &PublicationDecision::new())
                .map(crate::session::settled_manifest_attempt)
                .unwrap(),
            PublicManifestCommit::Durable
        ));
    });
    resident.set_run_context(public_context).unwrap();
    drop(checked);
    drop(execution);
    resident.retire_scope(private);
}
