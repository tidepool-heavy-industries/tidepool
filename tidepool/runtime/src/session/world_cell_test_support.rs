//! Shared captured-source and World admission setup for resident runtime tests.

use super::*;
use crate::session::{
    resident_cell_check_template, resident_workbench_templates, turn, SourceImports,
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
) where
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
    source_owner: &Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
    source: &str,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
) -> Result<Arc<crate::session::RuntimeCellAdmission>, crate::session::WorldInspectionError>
where
    H: crate::DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let specification = fixture_cell_recipe(recipe, source, &SourceImports::new());
    let original = source_owner.original_source().clone();
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
