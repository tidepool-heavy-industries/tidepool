//! Checked declaration shadowing keeps the earlier nominal type private.

use super::*;
use crate::session::{
    run_inspections, CertifiedDeclarationPublication, InspectionQuery, InspectionRequest,
    InspectionResult, ModuleEnv, PublicManifestCommit, PublicationDecision, RecoveryPublicOwner,
    SessionId, SessionLib,
};
use crate::CompileError;
use std::collections::BTreeSet;
use tidepool_repr::SessionModule;

#[derive(Clone)]
struct QuietOutput;

impl OutputSink for QuietOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

type TestSession = ResidentSession<frunk::HNil, QuietOutput>;

fn compile_single(
    resident: &mut TestSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
    execution: &Arc<crate::session::PrivateExecutionAdmission>,
    text: &str,
) -> (
    turn::CellCheck,
    Arc<crate::session::RuntimeCheckedItemAdmission>,
) {
    let admission =
        plan_fixture_cell_for(resident, source, recipe, text, execution.clone()).unwrap();
    let original = source.original_source().clone();
    let settled = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, report| turn::prepare_cell_program_admitted(owner, admission, report),
        )
        .unwrap();
    let (checked, program) = turn::finish_cell_program_admitted(settled).unwrap();
    assert_eq!(checked.items.len(), 1);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let prefix = resident.begin_cell_program(program).unwrap().unwrap();
    let item = checked.checked_item(0).unwrap();
    let admission = source_action_with_original(resident, original, |resident, _| {
        resident.admit_checked_item(prefix, item)
    })
    .unwrap();
    (checked, admission)
}

fn begin_execution(
    resident: &mut TestSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
) -> Arc<crate::session::PrivateExecutionAdmission> {
    Arc::new(
        source_action_with_original(resident, source.original_source().clone(), |resident, _| {
            resident.begin_private_execution(source.scope())
        })
        .unwrap(),
    )
}

#[test]
fn paired_join_compiles_source_hidden_checked_bind_in_fresh_worker() {
    tidepool_testing::eval_harness::require_extract();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(SessionId(994), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/source-hidden").unwrap(),
        1,
    )
    .unwrap();
    assert!(matches!(
        resident
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &source);
    let execution = Arc::new(
        source_action_with_original(
            &mut resident,
            source.original_source().clone(),
            |resident, _| resident.begin_durable_private_execution(&owner, public),
        )
        .unwrap(),
    );
    let private = execution.private_scope();
    let (original_generation, declarations, _) = execute_fixture_cell_for(
        &mut resident,
        &source,
        &recipe,
        &execution,
        include_str!("../fixtures/exact-join-original.hs"),
    );
    assert_eq!(declarations, vec![original_generation]);
    let original = resident
        .state
        .lib()
        .log
        .certified_authored_arc_at(original_generation)
        .unwrap();
    // A checked declaration shadow advances the World. Raw retraction does not
    // issue completed World progress and is covered by the publication controls.
    let (_, replacements, _) = execute_fixture_cell_for(
        &mut resident,
        &source,
        &recipe,
        &execution,
        "data HiddenResult = PublicResult deriving Show",
    );
    assert_eq!(replacements.len(), 1);
    let frozen = resident.freeze_private_execution(&execution).unwrap();
    let certified = source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            resident
                .state
                .restage_declaration_publication(owner, frozen)
                .unwrap()
                .certify(report)
        },
    )
    .unwrap();
    let CertifiedDeclarationPublication::Accepted(accepted) = certified else {
        panic!("completed declaration shadow must publish")
    };
    assert!(matches!(
        resident
            .state
            .attempt_public_manifest(accepted.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident.retire_scope(private);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();

    let original_path = root
        .path()
        .join(SessionModule::lib(original_generation).relative_hs_path());
    assert!(original_path.exists());
    std::fs::remove_file(&original_path).unwrap();
    let interface_path = original_path.with_extension("hi");
    if interface_path.exists() {
        std::fs::remove_file(interface_path).unwrap();
    }
    let view = resident.compile_view_in(public).unwrap();
    let context = view.exact_declaration_context().unwrap();
    let library_import = view.library_import_module().unwrap();
    assert_ne!(
        view.library(),
        Some(SessionModule::lib(original_generation))
    );
    assert_ne!(view.library().unwrap().module_name(), library_import);
    assert!(context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == library_import));
    assert!(context
        .lexical_graph()
        .iter()
        .all(|node| node.owner.module != original.product().owner().module));
    let original_artifact = context.authored_native_root(original_generation.0).unwrap();
    let issued_groups = original
        .artifact_view()
        .selected_native_groups()
        .into_iter()
        .filter(|key| key.artifact == original_artifact)
        .collect::<BTreeSet<_>>();
    assert!(!issued_groups.is_empty());
    assert!(context
        .artifact_view()
        .selected_native_groups()
        .iter()
        .all(|key| key.artifact != original_artifact));
    let published_digest = context.semantic_sha256();

    // This compiler-only observation deliberately starts a fresh direct worker.
    // World execution below continues through its actual resident endpoint.
    let socket = std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
    std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
    let imports = view.turn_imports(&SourceImports::new());
    let include = view.include_paths(&recipe.include);
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let queries = [
        InspectionQuery::TypeOf("PublicResult :: HiddenResult".into()),
        InspectionQuery::TypeOf("answer (41 :: Int)".into()),
        InspectionQuery::TypeOf("makeResult (41 :: Int)".into()),
        InspectionQuery::TypeOf("makeResult (41 :: Int) :: HiddenResult".into()),
    ];
    let inspected = tidepool_testing::with_settlement(|report| {
        run_inspections(
            InspectionRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                preamble: &recipe.preamble,
                imports: &imports,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                queries: &queries,
                effects: Some(&crate::session::HaskellTypeSource::from(
                    recipe.row.as_str(),
                )),
            },
            report,
        )
    });
    let hidden_imports = format!(
        "{imports}\nqualified {} as HiddenOriginal",
        original.product().owner().module
    );
    let hidden_queries = [InspectionQuery::TypeOf(
        "HiddenOriginal.answer (41 :: Int)".into(),
    )];
    let hidden = tidepool_testing::with_settlement(|report| {
        run_inspections(
            InspectionRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                preamble: &recipe.preamble,
                imports: &hidden_imports,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                queries: &hidden_queries,
                effects: Some(&crate::session::HaskellTypeSource::from(
                    recipe.row.as_str(),
                )),
            },
            report,
        )
    });
    match socket {
        Some(socket) => std::env::set_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV, socket),
        None => {}
    }
    let inspected = inspected.expect("fresh direct worker reads the retained joined interfaces");
    assert!(matches!(&inspected[0], InspectionResult::Type { .. }));
    assert!(matches!(&inspected[1], InspectionResult::Type { display, .. } if display == "Int"));
    assert!(matches!(&inspected[2], InspectionResult::Type { .. }));
    assert!(
        matches!(&inspected[3], InspectionResult::Rejected { .. }),
        "the earlier nominal type is distinct from its public replacement"
    );
    match hidden {
        Err(CompileError::SourceAdmission(failure)) => {
            assert_eq!(failure.owner.unit, original.product().owner().unit);
            assert_eq!(failure.owner.module, original.product().owner().module);
            assert_eq!(failure.reason, tidepool_toolchain::SourceAdmissionRefusal::HiddenOriginalPromotion);
        }
        Err(CompileError::Diagnostics(diagnostics) | CompileError::InputRejected(diagnostics)) => {
            assert!(diagnostics.iter().any(|diagnostic| diagnostic.message.contains(&original.product().owner().module)), "refusal names the attempted hidden owner");
        }
        Ok(results) => assert!(results.iter().all(|result| matches!(result, InspectionResult::Rejected { diagnostic } if diagnostic.contains(&original.product().owner().module)))),
        other => panic!("hidden original import must reach lexical refusal: {other:?}"),
    }
    eprintln!("fresh direct worker: retained interface succeeds; replacement nominal and hidden owner refuse");

    let binding = begin_execution(&mut resident, &source);
    let (checked, reservation) = compile_single(
        &mut resident,
        &source,
        &recipe,
        &binding,
        "let result = makeResult (41 :: Int)",
    );
    let item = checked.checked_item(0).unwrap();
    assert!(item.signatures()[0]
        .names()
        .iter()
        .any(|name| name.module() == original.product().owner().module
            && name.occurrence() == "HiddenResult"));
    let turn::TurnResult::Bind {
        bound, compiled, ..
    } = turn::consume_cell_program_item(reservation.clone()).unwrap()
    else {
        panic!("expected checked bind")
    };
    assert_eq!(bound[0].name, "result");
    assert!(bound[0].type_display.ends_with("HiddenResult"));
    let certification = compiled.certification.as_ref().unwrap();
    assert!(certification
        .checked_execution()
        .unwrap()
        .shares_target(&compiled.prepared));
    assert!(certification
        .groups
        .iter()
        .any(|group| group.owner() == original.product().owner()));
    let mut edited = bound[0].clone();
    edited.name = "edited".into();
    assert!(source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            resident.run_bind_with_sites(
                "edited",
                compiled.code(),
                &edited,
                reservation.generation(),
                report,
            )
        }
    )
    .is_err());
    assert_eq!(
        reservation
            .prefix()
            .snapshot()
            .compiler_prefix()
            .next_item(),
        0
    );
    let outcome = source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            resident.run_bind_with_sites(
                "checked",
                compiled.code(),
                &bound[0],
                reservation.generation(),
                report,
            )
        },
    )
    .unwrap();
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
    assert_eq!(
        reservation
            .prefix()
            .snapshot()
            .compiler_prefix()
            .next_item(),
        1
    );
    assert!(source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            resident.run_bind_with_sites(
                "replayed",
                compiled.code(),
                &bound[0],
                reservation.generation(),
                report,
            )
        }
    )
    .is_err());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();

    let expression = begin_execution(&mut resident, &source);
    let (checked, reservation) = compile_single(
        &mut resident,
        &source,
        &recipe,
        &expression,
        "makeResult (42 :: Int)",
    );
    assert!(checked.checked_item(0).unwrap().signatures()[0]
        .names()
        .iter()
        .any(|name| name.module() == original.product().owner().module
            && name.occurrence() == "HiddenResult"));
    let turn::TurnResult::Bind {
        bound, compiled, ..
    } = turn::consume_cell_program_item(reservation.clone()).unwrap()
    else {
        panic!("expected checked expression capture")
    };
    let outcome = source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            resident.run_observation_with_sites(
                compiled.code(),
                &bound[0],
                reservation.generation(),
                false,
                report,
            )
        },
    )
    .unwrap();
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
    assert_eq!(
        reservation
            .prefix()
            .snapshot()
            .compiler_prefix()
            .next_item(),
        1
    );
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();

    let answer = begin_execution(&mut resident, &source);
    let (_, reservation) = compile_single(
        &mut resident,
        &source,
        &recipe,
        &answer,
        "answer (41 :: Int)",
    );
    let turn::TurnResult::Bind {
        bound, compiled, ..
    } = turn::consume_cell_program_item(reservation.clone()).unwrap()
    else {
        panic!("expected answer observation")
    };
    let ResidentOutcome::Completed { result, .. } = source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            resident.run_observation_with_sites(
                compiled.code(),
                &bound[0],
                reservation.generation(),
                false,
                report,
            )
        },
    )
    .unwrap() else {
        panic!("retained original answer must execute")
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
    assert!(!original_path.exists());
    assert_eq!(context.semantic_sha256(), published_digest);
    assert!(view.is_current_for(&resident.compile_view_in(public).unwrap()));
    eprintln!("resident World execution: hidden nominal bind and expression complete; retained answer is 42");
}
