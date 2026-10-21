//! Finite source publication and later cells use the actual B source owner.
use super::*;

#[test]
#[ignore = "requires the rooted runtime_package_source_history_test resources"]
fn finite_source_original_feeds_public_world_cells_after_prior_cutoff() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let authored = root.path().join("authored");
    std::fs::create_dir(&authored).unwrap();
    std::fs::write(
        authored.join("PackageCafIsolation.hs"),
        include_str!("../../../tests/fixtures/PackageCafIsolation.hs"),
    )
    .unwrap();
    let mut includes = effects.include_paths().to_vec();
    includes.push(authored);
    let recipe = InputRecipe::capture(
        insert_preamble_imports(
            effects.preamble(),
            "qualified PackageCafIsolation as PackageProbe",
        ),
        effects.row().into(),
        includes,
    );
    let library = SessionLib::open(
        SessionId(1797),
        root.path().join("session"),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    let mut resident = TestSession::unbootstrapped(
        frunk::HNil,
        EmptyOutput,
        crate::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    resident
        .set_actor_execution(
            resident.public_context(),
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .unwrap();
    let templates = [turn::TurnTemplate {
        kind: crate::session::TemplateSelector::Expr,
        source: crate::session::assemble_expression_module(
            &recipe.preamble,
            "__result",
            &recipe.row,
            "{{TURN}}",
            crate::session::ExpressionLift::Effectful,
        ),
    }];
    let include = recipe
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let finite_root = root.path().join("finite");
    std::fs::create_dir(&finite_root).unwrap();
    let entry = tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: "pure (if PackageProbe.packageProbe 0 > 0 then (42 :: Int) else 0)",
                templates: &templates,
                include: &include,
                session_root: &finite_root,
                inject_modules: &[],
                gen: 1,
                verdict: None,
                target: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .expect("compile the real finite native source through the paired output owner");
    let prepared = crate::session::PreparedSourceEntry::prepare(
        Arc::new(compiled(entry)),
        Arc::new(tidepool_codegen::prepared_program::ImageRegistry::new()),
    )
    .expect("close the actual finite source entry without ambient bindings");
    let selected = prepared
        .published_source_original_selection(
            prepared
                .ordinary_original_compile_input()
                .original_input_identity(),
            &tidepool_toolchain::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "PackageCafIsolation".into(),
            },
        )
        .expect("the SAME prepared output issues its source-original selection");

    let source_a = resident.public_source.clone();
    let unpublished_b = resident.mint_unpublished_notebook_scope(None).unwrap();
    let source_b = unpublished_b.lease().clone();
    assert_ne!(source_a.scope(), source_b.scope());
    let installation_a = install_package_source(&mut resident, &prepared, &source_a);
    let installation_b = install_package_source(&mut resident, &prepared, &source_b);
    assert_ne!(installation_a.scope(), installation_b.scope());

    // Public B is its actual issued lease, not installer I_B or a named substitute.
    resident.public_source = source_b.clone();
    resident.set_run_context(resident.public_context()).unwrap();
    let public_b = source_b.scope();
    let staged = source_action(&mut resident, |resident, _| {
        resident.stage_published_source_originals_in(public_b, selected)
    })
    .unwrap();
    source_action(&mut resident, |resident, _| {
        resident.publish_source_originals(staged)
    })
    .unwrap();
    resident.bootstrap_world_in(&recipe, &source_b);
    public_package_cell(
        &mut resident,
        &recipe,
        "packageFirst",
        "if PackageProbe.packageProbe 0 > 0 then (42 :: Int) else 0",
        42,
    );
    public_package_cell(
        &mut resident,
        &recipe,
        "packageSecond",
        "P.fst packageFirst + if PackageProbe.packageSibling '\\NUL' > 0 then 1 else 0",
        43,
    );
    let cutoff = source_a.source_admission().fence_if_idle().unwrap();
    assert!(cutoff
        .retire(
            crate::session::NotebookGenerationId::INITIAL
                .next()
                .unwrap()
        )
        .is_ok());
    assert!(matches!(
        source_a.admit_source(),
        Err(crate::session::SourceAdmissionError::Closed { .. })
    ));
    public_package_cell(
        &mut resident,
        &recipe,
        "packageAfterCutoff",
        "P.fst packageSecond + if PackageProbe.packageProbe 2 > 0 then 1 else 0",
        44,
    );
    drop(resident);
    drop(installation_b);
    drop(installation_a);
    drop(unpublished_b);
    drop(source_a);
}

fn public_package_cell(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    name: &str,
    expression: &str,
    expected: i64,
) {
    let public = resident.public_context();
    let execution = begin_fixture_execution(resident);
    let private = execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: private,
            ..public
        })
        .unwrap();
    let source = format!("{name} <- pure (({expression}), T.pack (P.show ({expression})))");
    let (bound, compiled, reservation) =
        compile_checked_binding(resident, recipe, &source, execution.clone());
    let [binder] = bound.as_slice() else {
        panic!("one actual package-using public binder")
    };
    let completed = source_action(resident, |resident, settlement| {
        resident.run_bind_with_sites(
            name,
            compiled.code(),
            binder,
            reservation.generation(),
            settlement,
        )
    })
    .unwrap();
    let ResidentOutcome::Completed { result, .. } = completed else {
        panic!("the checked public-B package cell must execute")
    };
    assert_eq!(
        result.to_json(),
        serde_json::json!([expected, expected.to_string()])
    );
    publish_checked_fixture(resident, &execution, 1);
    resident.set_run_context(public).unwrap();
    drop(compiled);
    drop(reservation);
    drop(execution);
    resident.retire_scope(private);
}

fn install_package_source(
    resident: &mut TestSession,
    prepared: &crate::session::PreparedSourceEntry,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
) -> Arc<crate::session::RuntimeLexicalScopeLease> {
    let capsule = source_action_with_original(
        &mut resident.resident,
        source.original_source().clone(),
        |resident, _| resident.prepare_source_startup_entry(prepared),
    )
    .unwrap();
    let installation_b = capsule.installation_scope().clone();
    assert_ne!(installation_b.scope(), source.scope());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: installation_b.scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let completed = source_action_with_original(
        &mut resident.resident,
        source.original_source().clone(),
        |resident, settlement| resident.run_source_startup_entry(capsule, settlement),
    )
    .unwrap();
    let ResidentOutcome::Completed { result, .. } = completed else {
        panic!("the genuine source startup must complete its pure package demand");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));

    installation_b
}
