//! Small actual World history before actor startup and scheduling are involved.

use super::*;
use std::collections::BTreeMap;
use tidepool_repr::native_code::NativeCodeBody;
use tidepool_toolchain::linked_native::AcquiredNativeImport;

#[test]
fn declarations_precede_first_native_demands_and_display_without_replaying_effects() {
    let subscriber = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .finish();
    let _tracing = tracing::subscriber::set_default(subscriber);
    let mut session = SemanticSession::durable();
    // The independent oracle uses ordinary values, not compiler graph identities
    // or the implementation of either captured helper.
    let expected = ghc_trace(
        "segmentRecord 11\nlet seed = (41 :: Int)\nsegmentRecord 12\nlet plain = seed\nsegmentRecord 13\nlet text = length \"forty-one\"\nsegmentRecord text\nsegmentRecord (plain + text)",
    );
    assert_eq!(expected, [11, 12, 13, 9, 50]);
    let before_effects = session.trace.clone();
    let imports = SourceImports::from_specs([
        "qualified SegmentProbeSupport as Carrier",
        "qualified Tidepool.Inspection.Display as StageDisplay",
    ]);
    session
        .execute_observed(
            "staged_world_first_demands",
            include_str!("fixtures/staged-world-first-demands.hs"),
            CellDeclarationExpectation::Total(2),
            SemanticChecks::ItemOrder { future_item: 4 },
            &imports,
            None,
            |program| {
                assert!(
                    before_effects.lock().is_empty(),
                    "actual compiler preparation and END cannot execute effects"
                );
                assert_eq!(program.items().len(), 8);
                for index in [1, 3] {
                    assert!(program.items()[index].native().is_none());
                    let declaration = program.items()[index]
                        .checked_item()
                        .planned_declaration()
                        .expect("each declaration retains its actual source certificate");
                    let owner = declaration.product().owner();
                    let (_, imports) = declaration
                        .original_home_imports()
                        .find(|(importer, _)| {
                            importer.unit == owner.unit && importer.module == owner.module
                        })
                        .expect("the original keeps its authenticated compiler dependencies");
                    assert!(
                        imports.iter().any(|imported| {
                            imported.unit == "main" && imported.module == "Tidepool.TextFormat"
                        }),
                        "protected TextFormat remains a compiler dependency at item {index}"
                    );
                    let source = declaration
                        .extend_source_surface(&Default::default())
                        .unwrap();
                    assert!(
                        !source.roots().iter().any(|imported| {
                            imported.unit == "main" && imported.module == "Tidepool.TextFormat"
                        }),
                        "compiler support must not become a lexical root at item {index}"
                    );
                }
                for (index, selected_value, obsolete_value) in [(2, 1, None), (4, 2, Some(1))] {
                    let original = program.items()[index]
                        .native_products()
                        .unwrap()
                        .original_execution_context()
                        .unwrap();
                    let selected = tidepool_repr::SessionModule::val(
                        tidepool_repr::Generation(selected_value),
                    )
                    .module_name();
                    let roles = original.compiler_input_roles();
                    assert!(
                        original.artifact_view().descriptors().iter().any(|descriptor| {
                            descriptor.owner.unit == "main"
                                && descriptor.owner.module == selected
                                && roles.contains(
                                    &tidepool_toolchain::artifact_inventory::CompilerInputRole::InterfaceOnly {
                                        interface: descriptor.id,
                                    },
                                )
                        }),
                        "item {index} selects the prior stage's produced {selected} as a TYPE input"
                    );
                    assert!(
                        !original.lexical_graph().iter().any(|node| {
                            node.owner.unit == "main" && node.owner.module == selected
                        }),
                        "same-cell produced TYPE input {selected} does not create an instance-scope root"
                    );
                    if let Some(generation) = obsolete_value {
                        let obsolete = tidepool_repr::SessionModule::val(
                            tidepool_repr::Generation(generation),
                        )
                        .module_name();
                        assert!(
                            !original.lexical_graph().iter().any(|node| {
                                node.owner.unit == "main" && node.owner.module == obsolete
                            }),
                            "a previous value is not a compiler-instance obligation of a later stage"
                        );
                    }
                }
                let initial_context = program.items()[0]
                    .native_products()
                    .unwrap()
                    .original_execution_context()
                    .unwrap();
                let originals = initial_context.recovery_products();
                let available = originals
                    .iter()
                    .find(|product| product.owner().module == "SegmentProbeSupport")
                    .expect("initial compilation retains the complete immutable helper carrier");
                let bodies = tidepool_repr::execution_schema::parse_module_products(
                    available.product_bytes(),
                    &tidepool_toolchain::prepared_artifact::production_requirements().unwrap(),
                    tidepool_repr::execution_schema::InventoryDecodeLimits::default(),
                )
                .unwrap();
                let [body] = bodies.as_slice() else {
                    panic!("one authenticated helper carrier");
                };
                for name in ["carrierBaseline", "lateCarrierTextLength"] {
                    assert!(
                        body.groups.iter().any(|group| group
                            .binders()
                            .iter()
                            .any(|binder| binder.occurrence == name)),
                        "{name} is physically available before its first demand"
                    );
                }
                let mut demanded = BTreeMap::<usize, BTreeMap<String, usize>>::new();
                for (index, item) in program.items().iter().enumerate() {
                    let Some(products) = item.native_products() else {
                        continue;
                    };
                    let target = products
                        .linked_native_target()
                        .expect("an executable item retains its actual linked target");
                    let mut helpers = BTreeMap::new();
                    for acquired in target.acquired() {
                        let NativeCodeBody::Home(product) = acquired.code().body() else {
                            continue;
                        };
                        if product.owner().module != "SegmentProbeSupport" {
                            continue;
                        }
                        assert_eq!(product.owner(), available.owner());
                        for (binder, _) in product.exported_values() {
                            if matches!(
                                binder.occurrence.as_str(),
                                "carrierBaseline" | "lateCarrierTextLength"
                            ) {
                                let packages = acquired
                                    .imports()
                                    .filter(|slot| {
                                        matches!(
                                            slot,
                                            AcquiredNativeImport::Code { acquired, .. }
                                                if matches!(acquired.code().body(), NativeCodeBody::Package(_))
                                        )
                                    })
                                    .count();
                                assert!(helpers
                                    .insert(binder.occurrence.clone(), packages)
                                    .is_none());
                            }
                        }
                    }
                    demanded.insert(index, helpers);
                }
                assert_eq!(
                    demanded[&2].get("carrierBaseline"),
                    Some(&0),
                    "the actual package-free helper is demanded after its declaration prefix"
                );
                assert!(
                    demanded[&4]
                        .get("lateCarrierTextLength")
                        .is_some_and(|slots| *slots > 0),
                    "the actual package-bearing helper must exercise ordered package slots"
                );
                for (first, helper) in [(2, "carrierBaseline"), (4, "lateCarrierTextLength")] {
                    for (index, helpers) in &demanded {
                        if *index < first {
                            assert!(
                                !helpers.contains_key(helper),
                                "available helper {helper} must not be selected by earlier item {index}"
                            );
                        }
                    }
                }
            },
        )
        .unwrap();
    assert_eq!(session.observed(), expected);
    for name in ["stagedSeed", "stagedPlain", "stagedText"] {
        assert!(session
            .resident
            .current_binding_in(session.public, name)
            .is_some());
    }
    // The existing executor has published and retired the private scope. A new
    // admitted World cell must consume the surviving values without replay.
    session
        .execute(
            "staged_world_fresh_consumer",
            "segmentRecord (stagedPlain + stagedText + 1)",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [11, 12, 13, 9, 50, 51]);
    session.assert_no_compiler_since_last_effect();

    session
        .execute_observed(
            "staged_world_cold_original",
            "stagedColdAnswer :: Int -> Int\nstagedColdAnswer value = Carrier.carrierBaseline value + 1\nsegmentRecord (stagedColdAnswer 51)",
            CellDeclarationExpectation::Total(1),
            SemanticChecks::None,
            &imports,
            None,
            |_| {},
        )
        .unwrap();
    assert_eq!(session.observed(), [11, 12, 13, 9, 50, 51, 52]);

    let mut cold = session.reopen_without_authored_source();
    for name in ["stagedSeed", "stagedPlain", "stagedText"] {
        assert!(
            cold.resident
                .current_binding_in(cold.public, name)
                .is_none(),
            "cold recovery must not reconstruct the lost heap binding {name}"
        );
    }
    assert_eq!(cold.observed(), [11, 12, 13, 9, 50, 51, 52]);
    cold.execute(
        "staged_world_cold_consumer",
        "segmentRecord (stagedColdAnswer 52)",
        0,
    )
    .unwrap();
    assert_eq!(cold.observed(), [11, 12, 13, 9, 50, 51, 52, 53]);
    cold.assert_no_compiler_since_last_effect();
}

impl SemanticSession {
    fn reopen_without_authored_source(self) -> Self {
        let Self {
            resident,
            public_source,
            recipe,
            effects,
            recovery,
            trace,
            compiler_counts,
            cancel_after_record,
            _root: root,
            ..
        } = self;
        let recovery = recovery.expect("cold history starts with durable publication");
        let authority = recovery.owner.clone();
        drop(resident);
        drop(public_source);
        drop(recipe);
        drop(recovery);
        let authored = root.path().join("Tidepool/Session");
        assert!(
            authored.is_dir(),
            "the history must have written real source"
        );
        std::fs::remove_dir_all(&authored).unwrap();

        let recovery = crate::session::recovery_test_fixture::RecoveryFixture::reopen(authority);
        let mut includes = effects.include_paths().to_vec();
        includes.push(root.path().join("support"));
        let recipe = InputRecipe::capture(
            effects.preamble().to_owned(),
            effects.row().to_owned(),
            includes,
        );
        let mut lib = SessionLib::open(
            SessionId(1011),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(recipe.include.clone());
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
            .unwrap();
        let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = persistent
            .recover_public_scope(&semantic_recovery_owner())
            .unwrap();
        // Restored bytes carry the exact declaration selection. A fresh live
        // notebook lease owns this machine's source admission and World setup.
        let public_source = persistent
            .adopt_initial_notebook_scope_with_source(public, None)
            .unwrap();
        let mut resident = ResidentSession::from_persistent_for_test(
            frunk::hlist![ProbeHandler {
                trace: trace.clone(),
                raw_result: None,
                compiler_counts: compiler_counts.clone(),
                cancel_after_record: cancel_after_record.clone(),
            }],
            QuietOutput,
            persistent,
        );
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: public,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        bootstrap_world_for(&mut resident, &recipe, &public_source);
        assert!(
            !authored.exists(),
            "World setup must use retained declarations"
        );
        Self {
            resident,
            public,
            public_source,
            recipe,
            trace,
            compiler_counts,
            history_compiler_census: None,
            cancel_after_record,
            effects,
            recovery: Some(recovery),
            _root: root,
        }
    }
}
