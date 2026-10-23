//! Shared captured-source and World admission setup for resident runtime tests.

use super::*;
use crate::session::{
    resident_cell_check_template, resident_workbench_templates, turn, PrivateExecutionAdmission,
    RuntimeCellAdmission, RuntimeLexicalScopeLease, RuntimeOriginalSource, SourceImports,
    WorldInspectionError,
};

/// The same owned source bytes and includes supply the admission and compiler.
pub(in crate::session) struct InputRecipe {
    pub(in crate::session) preamble: String,
    pub(in crate::session) row: String,
    pub(in crate::session) include: Vec<PathBuf>,
    pub(in crate::session) reference: crate::session::GenerationArtifactReference,
    pub(in crate::session) sources:
        Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>,
}

impl InputRecipe {
    pub(in crate::session) fn capture(
        preamble: String,
        row: String,
        include: Vec<PathBuf>,
    ) -> Self {
        use std::io::Read;
        use tidepool_toolchain::compiler_world::{
            CapturedCompilerSourceRoot, CapturedCompilerSources,
        };
        let owner = Arc::new(tempfile::tempdir().unwrap());
        let mut roots = Vec::new();
        let mut paths = Vec::new();
        let mut remaining = CapturedCompilerSources::capture_byte_allowance().unwrap();
        for (ordinal, input) in include.iter().enumerate() {
            remaining = remaining
                .checked_sub(4)
                .expect("source root framing fits capture allowance");
            let files = tidepool_toolchain::cache::source_root_manifest(input).unwrap();
            let destination = owner.path().join(format!("root-{ordinal}"));
            std::fs::create_dir_all(&destination).unwrap();
            let mut bodies = std::collections::BTreeMap::new();
            for (relative, digest) in &files {
                let path = input.join(relative);
                let length = std::fs::metadata(&path).unwrap().len();
                let framed = 8u64
                    .checked_add(u64::try_from(relative.as_os_str().len()).unwrap())
                    .and_then(|n| n.checked_add(length))
                    .unwrap();
                remaining = remaining
                    .checked_sub(framed)
                    .expect("fixture source capture fits compiler allowance before retention");
                let mut body = Vec::new();
                std::fs::File::open(&path)
                    .unwrap()
                    .take(length.checked_add(1).unwrap())
                    .read_to_end(&mut body)
                    .unwrap();
                assert_eq!(body.len() as u64, length, "source changed during capture");
                assert_eq!(
                    blake3::hash(&body).to_hex().as_str(),
                    digest,
                    "source changed after manifest capture"
                );
                let target = destination.join(relative);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, &body).unwrap();
                bodies.insert(relative.clone(), Arc::<[u8]>::from(body));
            }
            roots.push(CapturedCompilerSourceRoot {
                manifest: tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)
                    .unwrap(),
                bodies,
            });
            paths.push(destination);
        }
        let manifests = roots
            .iter()
            .map(|root| root.manifest.clone())
            .collect::<Vec<_>>();
        let identity = tidepool_toolchain::cache::source_manifests_identity(
            b"runtime-activation-input-fixture",
            &manifests,
        );
        let reference = crate::session::GenerationArtifactReference::capture(
            serde_json::to_vec(&(&preamble, &row, identity)).unwrap(),
        );
        let sources = Arc::new(
            CapturedCompilerSources::from_captured_roots(
                roots,
                Arc::from(reference.bytes()),
                owner,
            )
            .unwrap(),
        );
        Self {
            preamble,
            row,
            include: paths,
            reference,
            sources,
        }
    }
}

pub(in crate::session) fn state_source_action<T>(
    state: &mut PersistentSession,
    original: Arc<RuntimeOriginalSource>,
    action: impl FnOnce(&mut PersistentSession) -> T,
) -> T {
    if crate::CompilerTransactionOrigin::current().is_some() {
        state
            .source_transaction
            .as_ref()
            .expect("fixture borrows its actual source transaction")
            .admit_original(&original, None)
            .unwrap();
        return action(state);
    }
    let settled = state
        .with_source_compiler_transaction(
            original,
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |state, _| action(state),
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    settled.into_outcome().action
}

/// Bootstrap the actual public World before beginning a substrate fixture's
/// private execution. The captured source owner, compiler END and installation
/// are the same production transitions used by resident callers.
pub(in crate::session) fn begin_state_fixture_execution(
    state: &mut PersistentSession,
    source: &Arc<RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
) -> Arc<PrivateExecutionAdmission> {
    let public = source.scope();
    bootstrap_state_world_for(state, recipe, source);
    let original = source.original_source().clone();
    let settled = state
        .with_source_compiler_transaction(
            original,
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |state, _| state.begin_private_execution(public),
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    Arc::new(settled.into_outcome().action.unwrap())
}

pub(in crate::session) fn bootstrap_state_world_for(
    state: &mut PersistentSession,
    recipe: &InputRecipe,
    source: &Arc<RuntimeLexicalScopeLease>,
) -> Arc<tidepool_toolchain::compiler_world::CompilerWorld> {
    let original = source.original_source().clone();
    let admitted = state
        .capture_inspection_world_in(source.scope(), &recipe.reference, &original, |_| false)
        .unwrap();
    let settled = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                crate::session::run_admitted_compiler_world(owner, admitted, || {
                    Ok(Some(recipe.sources.clone()))
                })
            },
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    let completed = crate::session::finish_admitted_compiler_world(settled).unwrap();
    assert!(state.install_compiler_world_association_if_current(
        &completed.association,
        &completed.custody,
        &completed.cancellation,
        &recipe.reference,
        &original
    ));
    completed.world().clone()
}

pub(in crate::session) fn plan_state_fixture_specification_for(
    state: &mut PersistentSession,
    execution: Arc<PrivateExecutionAdmission>,
    specification: Arc<tidepool_extract_cmd::WorldCellRecipe>,
) -> Result<Arc<RuntimeCellAdmission>, WorldInspectionError> {
    let original = execution
        .original_source()
        .map_err(crate::RuntimeError::from)?;
    let settled = state
        .with_source_compiler_transaction(
            original.clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |state, _| state.prepare_private_execution_snapshot(execution.clone()),
        )
        .map_err(crate::RuntimeError::from)?;
    assert!(settled.close().resources_retired());
    let snapshot = settled
        .into_outcome()
        .action
        .map_err(crate::RuntimeError::from)?;
    let settled = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                turn::prepare_world_cell_plan_admitted(owner, snapshot.world(), specification, None)
            },
        )
        .map_err(crate::RuntimeError::from)?;
    let plan = turn::finish_world_cell_plan_admitted(settled)?;
    let settled = state
        .with_source_compiler_transaction(
            original,
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |state, _| {
                state.admit_world_planned_cell_with_snapshot(execution, snapshot, plan, None)
            },
        )
        .map_err(crate::RuntimeError::from)?;
    assert!(settled.close().resources_retired());
    settled
        .into_outcome()
        .action
        .map_err(crate::RuntimeError::from)
        .map_err(Into::into)
}

pub(in crate::session) fn world_recipe(
    specification: &tidepool_toolchain::checked_cell::CellRecipe,
) -> Arc<tidepool_extract_cmd::WorldCellRecipe> {
    Arc::new(tidepool_extract_cmd::WorldCellRecipe {
        source: specification.cell_source().to_owned(),
        template: specification.template_source().to_owned(),
        turn_templates: specification.turn_templates().to_vec(),
    })
}

pub(in crate::session) fn source_action_with_original<H, O, T>(
    resident: &mut ResidentSession<H, O>,
    original: Arc<crate::session::RuntimeOriginalSource>,
    action: impl FnOnce(
        &mut ResidentSession<H, O>,
        &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> T,
) -> T
where
    H: crate::DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if crate::CompilerTransactionOrigin::current().is_some() {
        let owner = resident
            .state
            .source_transaction
            .as_ref()
            .expect("ambient compiler phase has its actual source owner");
        owner
            .admit_original(&original, None)
            .expect("nested fixture phase matches its held source owner");
        let mut settlement = |_close| {};
        return action(resident, &mut settlement);
    }
    let settled = resident
        .with_source_compiler_transaction(
            original,
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |resident, settlement| action(resident, settlement),
        )
        .expect("open the fixture compiler transaction");
    assert!(
        settled.close().resources_retired(),
        "fixture compiler transaction retained source custody: {:?}",
        settled.close()
    );
    settled.into_outcome().action
}

pub(in crate::session) fn bootstrap_world_for<H, O>(
    resident: &mut ResidentSession<H, O>,
    recipe: &InputRecipe,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
) -> Arc<tidepool_toolchain::compiler_world::CompilerWorld>
where
    H: crate::DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let scope = source.scope();
    let admitted = resident
        .capture_inspection_world_in(scope, &recipe.reference, source.original_source())
        .unwrap();
    let settled = source
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                crate::session::run_admitted_compiler_world(owner, admitted, || {
                    Ok(Some(recipe.sources.clone()))
                })
            },
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    let completed = crate::session::finish_admitted_compiler_world(settled).unwrap();
    assert!(resident.install_compiler_world_if_current(
        &completed,
        &recipe.reference,
        source.original_source()
    ));
    completed.world().clone()
}

pub(in crate::session) fn fixture_cell_recipe(
    recipe: &InputRecipe,
    source: &str,
    imports: &SourceImports,
) -> Arc<tidepool_extract_cmd::WorldCellRecipe> {
    use crate::session::TemplateSelector;
    let template =
        resident_cell_check_template(&recipe.preamble, &recipe.row, &imports.template_text());
    let templates =
        resident_workbench_templates(&recipe.preamble, &recipe.row, &imports.template_text());
    Arc::new(tidepool_extract_cmd::WorldCellRecipe {
        source: source.to_owned(),
        template,
        turn_templates: templates
            .into_iter()
            .map(|template| {
                let kind = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (kind.to_owned(), template.source)
            })
            .collect(),
    })
}

pub(in crate::session) fn plan_fixture_cell_for<H, O>(
    resident: &mut ResidentSession<H, O>,
    recipe: &InputRecipe,
    source: &str,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
) -> Result<Arc<crate::session::RuntimeCellAdmission>, crate::session::WorldInspectionError>
where
    H: crate::DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let specification = fixture_cell_recipe(recipe, source, &SourceImports::new());
    plan_fixture_specification_for(resident, specification, execution)
}

pub(in crate::session) fn plan_fixture_specification_for<H, O>(
    resident: &mut ResidentSession<H, O>,
    specification: Arc<tidepool_extract_cmd::WorldCellRecipe>,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
) -> Result<Arc<crate::session::RuntimeCellAdmission>, crate::session::WorldInspectionError>
where
    H: crate::DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let original = execution
        .original_source()
        .map_err(crate::RuntimeError::from)?;
    let preparation = source_action_with_original(resident, original.clone(), |resident, _| {
        resident.prepare_private_execution(execution.clone(), None)
    })
    .map_err(crate::RuntimeError::from)?;
    let (snapshot, operand) = preparation.into_parts();
    assert!(operand.is_none());
    let settled = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                turn::prepare_world_cell_plan_admitted(owner, snapshot.world(), specification, None)
            },
        )
        .map_err(crate::RuntimeError::from)?;
    let plan = turn::finish_world_cell_plan_admitted(settled)?;
    source_action_with_original(resident, original, |resident, _| {
        resident.admit_world_planned_cell_with_snapshot(execution, snapshot, plan, None)
    })
    .map_err(crate::RuntimeError::from)
    .map_err(Into::into)
}

/// Compile and execute every issued item; only actual completion advances World.
pub(in crate::session) fn execute_fixture_cell_for<H, O>(
    resident: &mut ResidentSession<H, O>,
    source_owner: &Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
    execution: &Arc<crate::session::PrivateExecutionAdmission>,
    source: &str,
) -> (
    crate::session::Generation,
    Vec<crate::session::Generation>,
    Arc<crate::session::RuntimeCheckedPrefix>,
)
where
    H: crate::DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let original = source_owner.original_source().clone();
    let admission = plan_fixture_cell_for(resident, recipe, source, execution.clone())
        .expect("plan the cell through its actual World");
    let settled = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, report| turn::prepare_cell_program_admitted(owner, admission, report),
        )
        .unwrap();
    let (checked, program) =
        turn::finish_cell_program_admitted(settled).expect("check the complete publication cell");
    let prefix = resident
        .begin_cell_program(program)
        .unwrap()
        .expect("nonempty publication cell");
    let previous = resident.run_context();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..previous
        })
        .unwrap();
    let mut declarations = Vec::new();
    let before_native = tidepool_extract_cmd::extract_spawn_count();
    for index in 0..checked.items.len() {
        let item = checked.checked_item(index).unwrap();
        let reservation = source_action_with_original(resident, original.clone(), |resident, _| {
            resident.admit_checked_item(prefix.clone(), item.clone())
        })
        .unwrap();
        if item.kind() == tidepool_toolchain::checked_cell::CheckedItemKind::Declaration {
            let committed =
                source_action_with_original(resident, original.clone(), |resident, _| {
                    resident.adopt_checked_declaration(reservation)
                })
                .unwrap();
            declarations.push(committed.generation);
        } else {
            let turn::TurnResult::Bind {
                bound, compiled, ..
            } = turn::consume_cell_program_item(reservation.clone()).unwrap()
            else {
                panic!("checked value must compile a binding");
            };
            assert_eq!(bound.len(), 1);
            let outcome =
                source_action_with_original(resident, original.clone(), |resident, report| {
                    if item.kind() == tidepool_toolchain::checked_cell::CheckedItemKind::Bind {
                        resident.run_bind_with_sites(
                            &bound[0].name,
                            compiled.code(),
                            &bound[0],
                            reservation.generation(),
                            report,
                        )
                    } else {
                        assert_eq!(
                            item.kind(),
                            tidepool_toolchain::checked_cell::CheckedItemKind::Expression
                        );
                        resident.run_observation_with_sites(
                        compiled.code(), &bound[0], reservation.generation(),
                        item.expression_lift().unwrap() == Some(
                            tidepool_toolchain::checked_cell::CheckedExpressionLift::Effectful,
                        ), report,
                    )
                    }
                })
                .unwrap();
            assert!(
                matches!(outcome, ResidentOutcome::Completed { .. }),
                "actual publication value must complete: {outcome:?}"
            );
            if item.kind() == tidepool_toolchain::checked_cell::CheckedItemKind::Bind {
                let visible = resident
                    .public_visibility_snapshot_in(execution.private_scope())
                    .unwrap();
                assert!(visible
                    .bindings
                    .iter()
                    .any(|(name, id)| name == &bound[0].name
                        && *id == SessionVarId::from_extract(bound[0].var_id)));
            }
        }
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), index + 1);
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            before_native,
            "execution consumes its sealed native program without recompilation"
        );
    }
    resident.set_run_context(previous).unwrap();
    (
        resident.state.lib().scope_tip(execution.private_scope()),
        declarations,
        prefix,
    )
}

#[path = "world_cell_test_support/source_hidden_join.rs"]
mod source_hidden_join;

#[path = "world_cell_test_support/publication_linearization.rs"]
mod publication_linearization;

#[path = "world_cell_test_support/publication_semantic_currency.rs"]
mod publication_semantic_currency;
