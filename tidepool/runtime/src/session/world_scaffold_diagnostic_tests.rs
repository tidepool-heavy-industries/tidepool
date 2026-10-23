//! Shipped scaffold and authored diagnostics through actual World preparation.

use super::*;
use crate::session::world_cell_test_support::{
    begin_state_fixture_execution, plan_state_fixture_specification_for,
};

fn prepare(
    recipe: &InputRecipe,
    specification: Arc<tidepool_extract_cmd::WorldCellRecipe>,
) -> Result<(CellCheck, SettledCellProgram), CompileError> {
    let root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        SessionId(1031),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    let mut state = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = state.mint_scope(ScopeId::ROOT).unwrap();
    let source = state.adopt_initial_notebook_scope(public).unwrap();
    let execution = begin_state_fixture_execution(&mut state, &source, recipe);
    let admission = plan_state_fixture_specification_for(&mut state, execution, specification)
        .map_err(|error| match error {
            crate::session::WorldInspectionError::Runtime(crate::RuntimeError::Compile(error))
            | crate::session::WorldInspectionError::Runtime(crate::RuntimeError::Session(
                crate::session::SessionError::Compile(error),
            )) => error,
            other => panic!("World admission failed outside compilation: {other:?}"),
        })?;
    tidepool_testing::with_settlement(|settlement| {
        compile_cell_program_admitted(admission, settlement)
    })
    .map_err(|failure| {
        assert!(failure.source_admission.is_none(), "{failure:?}");
        assert!(
            failure
                .compiler_close
                .as_ref()
                .is_some_and(|close| close.resources_retired()),
            "a GHC refusal must retain its confirmed outer close: {failure:?}"
        );
        failure.error
    })
}

#[test]
fn shipped_world_scaffold_scopes_equality_with_a_custom_prelude() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        format!(
            "{{-# OPTIONS_GHC -Werror=type-equality-out-of-scope #-}}\n{}",
            effects.preamble(),
        ),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let specification = fixture_cell_recipe(&recipe, "1 :: Int\n", &SourceImports::new());
    let mut unqualified = (*specification).clone();
    assert!(unqualified.template.contains("TidepoolWorkbenchEquality.~"));
    unqualified.template = unqualified
        .template
        .replace("TidepoolWorkbenchEquality.~", "~");
    // The immutable source recipe, submitted cell and turn templates are identical.
    // Only the equality operator in the shipped whole-cell scaffold changes.
    let error = prepare(&recipe, Arc::new(unqualified))
        .expect_err("the custom Prelude does not supply the unqualified equality operator");
    let CompileError::Diagnostics(diagnostics) = &error else {
        panic!("expected a genuine GHC equality-scope diagnostic: {error:?}");
    };
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == crate::diag::DiagnosticSeverity::Error
                && diagnostic.message.contains("operator is out of scope")
                && diagnostic.message.contains("equality constraint")
        }),
        "{error:?}"
    );
    for (cell, lift) in [
        ("1 :: Int\n", ExpressionLift::Pure),
        ("pure (1 :: Int)\n", ExpressionLift::Effectful),
    ] {
        let specification = fixture_cell_recipe(&recipe, cell, &SourceImports::new());
        let (checked, program) = prepare(&recipe, specification)
            .expect("the shipped scaffold scopes equality under the custom Prelude");
        assert_eq!(checked.expression_plans.len(), 1);
        assert_eq!(checked.expression_plans[0].lift, lift);
        assert!(checked.warnings.is_empty(), "{:?}", checked.warnings);
        program.validate_typed_entries().unwrap();
    }
}

#[test]
fn world_preparation_reports_authored_record_warning_without_imported_warnings() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let dependency = tempfile::tempdir().unwrap();
    std::fs::write(
        dependency.path().join("Prior.hs"),
        include_str!("fixtures/world-scaffold-prior.hs"),
    )
    .unwrap();
    let mut include = effects.include_paths().to_vec();
    include.insert(0, dependency.path().to_owned());
    let recipe = InputRecipe::capture(
        format!(
            "{{-# OPTIONS_GHC -Wmissing-fields #-}}\n{}",
            effects.preamble(),
        ),
        effects.row().to_owned(),
        include,
    );
    let imports = SourceImports::from_specs(["qualified Prior"]);
    let partial = fixture_cell_recipe(
        &recipe,
        include_str!("fixtures/world-scaffold-record-partial.hs"),
        &imports,
    );
    let (checked, program) = prepare(&recipe, partial)
        .expect("a missing record field is a warning on a valid authored declaration");
    assert_prior_emitted(&program);
    assert!(
        checked.warnings.iter().any(|warning| {
            warning.severity == crate::diag::DiagnosticSeverity::Warning
                && warning
                    .span
                    .as_ref()
                    .is_some_and(|span| span.file == "<cell>" && span.start_line == 2)
                && warning.message.contains("mergeSourceWorktree")
        }),
        "authored warnings: {:?}",
        checked.warnings
    );
    // Prior deliberately labels its own warning <cell> too: provenance, rather
    // than a matching rendered filename, must decide which warnings are authored.
    assert!(
        checked.warnings.iter().all(|warning| warning
            .span
            .as_ref()
            .is_some_and(|span| span.file == "<cell>")
            && !warning.message.contains("oldTail")),
        "dependency warnings leaked into the cell: {:?}",
        checked.warnings
    );
    let complete = fixture_cell_recipe(
        &recipe,
        include_str!("fixtures/world-scaffold-record-complete.hs"),
        &imports,
    );
    let (checked, program) = prepare(&recipe, complete).expect("complete authored record");
    assert_prior_emitted(&program);
    assert!(checked.warnings.is_empty(), "{:?}", checked.warnings);
}

fn assert_prior_emitted(program: &SettledCellProgram) {
    let emitted = program.world_program().emitted_native_modules().unwrap();
    assert!(
        emitted.iter().any(|stage| stage
            .owners()
            .iter()
            .any(|owner| owner.unit == "main" && owner.module == "Prior")),
        "the referenced imported warning fixture must actually compile into a native stage"
    );
    program.validate_typed_entries().unwrap();
}
