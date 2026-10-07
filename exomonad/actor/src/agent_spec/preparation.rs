//! Source-owned installer preparation; waiters do not own the shared task.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, CompiledTurn, ImageRegistry, PreparedSourceEntry,
    TurnClassification, TurnKind, TurnRequest, TurnResult,
};

use super::ResolvedSpec;

/// The acquisition that issued immutable installer readiness. Ready-cache
/// hits and joined waiters retain this original provenance.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolsetAcquisition {
    DeploymentOriginal { recipe: String, original: uuid::Uuid },
    FreshRunOriginal { recipe: String, original: uuid::Uuid },
    ExistingRunOriginal { recipe: String, original: uuid::Uuid },
    UnretainedCompilation { recipe: String },
}

impl ToolsetAcquisition {
    #[must_use]
    pub fn recipe(&self) -> &str {
        match self {
            Self::DeploymentOriginal { recipe, .. }
            | Self::FreshRunOriginal { recipe, .. }
            | Self::ExistingRunOriginal { recipe, .. }
            | Self::UnretainedCompilation { recipe } => recipe,
        }
    }

    /// Selection metadata is derived from the issuing acquisition, never an
    /// independent claim that compilation reused a deployment.
    #[must_use]
    pub fn completed_entry_selection(&self) -> Option<(&str, uuid::Uuid)> {
        match self {
            Self::DeploymentOriginal { recipe, original }
            | Self::FreshRunOriginal { recipe, original }
            | Self::ExistingRunOriginal { recipe, original } => Some((recipe, *original)),
            Self::UnretainedCompilation { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub(crate) struct InstallerRecipe {
    /// In-memory coalescing retains the exact issuer and source selection.
    /// Durable addresses are independent of a fresh run's issuer identity.
    #[serde(skip)]
    pub(crate) source_authority: [u8; 32],
    pub(crate) source_revision: String,
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) preamble: String,
    pub(crate) imports: String,
    pub(crate) entry: String,
    pub(crate) effects: Vec<crate::ActorEffectKey>,
}

pub(crate) struct PreparedToolset {
    pub(crate) entry: PreparedSourceEntry,
    pub(crate) entry_name: String,
    pub(crate) resolved: ResolvedSpec,
    pub(crate) source_revision: String,
    pub(crate) completed_selection: Option<(String, uuid::Uuid)>,
    /// Retains the published source owner for the complete installer lifetime.
    _source: crate::CheckpointSourceLayer,
    /// Nominal owners include producer and original canonical interface identity.
    _nominal_artifacts: Vec<tidepool_toolchain::artifact_inventory::ArtifactDescriptor>,
}

fn nominal_artifacts(
    compiled: &CompiledTurn,
    effects: &[crate::ActorEffectKey],
) -> Result<Vec<tidepool_toolchain::artifact_inventory::ArtifactDescriptor>, PreparationFailure> {
    use tidepool_toolchain::artifact_inventory::ArtifactKind;
    let descriptors = compiled.source_artifacts();
    let mut modules = vec!["Tidepool.Effects.Core", "Tidepool.Agent.Contract"];
    if effects.contains(&crate::ActorEffectKey::Replies) {
        modules.push("Tidepool.Agent.Reply.Internal");
    }
    if effects.contains(&crate::ActorEffectKey::Watches) {
        modules.push("Tidepool.Agent.Watch.Internal");
    }
    modules
        .into_iter()
        .map(|module| {
            let selected = descriptors
                .iter()
                .filter(|descriptor| {
                    descriptor.owner.module == module
                        && matches!(
                            descriptor.kind,
                            ArtifactKind::OriginalModule | ArtifactKind::CanonicalModuleInterface
                        )
                })
                .collect::<Vec<_>>();
            let Some(first) = selected.first() else {
                return Err(PreparationFailure::Source(format!(
                    "installer has no original nominal artifact for {module}",
                )));
            };
            if selected.iter().any(|descriptor| {
                descriptor.owner != first.owner
                    || descriptor.producer_sha256 != first.producer_sha256
                    || descriptor.interface_sha256 != first.interface_sha256
            }) {
                return Err(PreparationFailure::Source(format!(
                    "installer selects conflicting nominal artifacts for {module}",
                )));
            }
            Ok((**first).clone())
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) enum PreparationFailure {
    AbsentSelection { recipe: String },
    Admission(Arc<crate::ResidentActorWorkbenchError>),
    Source(String),
    Compiler(tidepool_toolchain::failclass::FailureEnvelope),
    Native(String),
}

struct PreparationTask {
    outcome: Mutex<Option<Result<Arc<PreparedToolset>, PreparationFailure>>>,
    completed: tokio::sync::Notify,
}

impl PreparationTask {
    async fn wait(&self) -> Result<Arc<PreparedToolset>, PreparationFailure> {
        loop {
            let completed = self.completed.notified();
            tokio::pin!(completed);
            completed.as_mut().enable();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome;
            }
            completed.await;
        }
    }
}

#[derive(Default)]
pub(crate) struct ToolsetPreparation {
    state: Mutex<PreparationState>,
    #[cfg(test)]
    fresh_launch_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Default)]
struct PreparationState {
    tasks: HashMap<InstallerRecipe, Arc<PreparationTask>>,
    ready_order: VecDeque<InstallerRecipe>,
}

const RETAINED_TOOLSETS: usize = 16;

#[derive(Clone, Copy)]
enum OriginalAcquisition {
    CompileIfAbsent,
    LoadCompleted,
}

impl ToolsetPreparation {
    /// Delay only a winning real compile; the hook grants no compiler authority.
    #[cfg(test)]
    pub(crate) fn observe_fresh_launch(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        let mut slot = self.fresh_launch_observer.lock();
        assert!(slot.is_none(), "fresh launch observer already installed");
        *slot = Some(observer);
    }

    fn lookup(&self, recipe: &InstallerRecipe) -> (Arc<PreparationTask>, bool) {
        let mut state = self.state.lock();
        match state.tasks.get(recipe).cloned() {
            Some(task) => {
                if state.ready_order.contains(recipe) {
                    state.ready_order.retain(|key| key != recipe);
                    state.ready_order.push_back(recipe.clone());
                }
                (task, false)
            }
            None => {
                let task = Arc::new(PreparationTask {
                    outcome: Mutex::new(None),
                    completed: tokio::sync::Notify::new(),
                });
                state.tasks.insert(recipe.clone(), Arc::clone(&task));
                (task, true)
            }
        }
    }

    pub(crate) async fn prepare(
        self: &Arc<Self>,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        recipe: InstallerRecipe,
        resolved: ResolvedSpec,
        source: crate::CheckpointSourceLayer,
        registry: Arc<ImageRegistry>,
    ) -> Result<Arc<PreparedToolset>, PreparationFailure> {
        let (task, launch) = self.lookup(&recipe);
        if launch {
            let completed_original = match selected_original_present(&recipe, &source) {
                Ok(present) => present,
                Err(error) => {
                    self.settle(recipe, &task, Err(error));
                    return task.wait().await;
                }
            };
            let compiler_work = if completed_original {
                None
            } else {
                let admitted = crate::resident_workbench::CompilerCloseOwner::current()
                    .and_then(|owner| owner.register_work());
                match admitted {
                    Ok(ticket) => Some((
                        ticket,
                        tidepool_runtime::CompilerTransactionCancellation::new(),
                    )),
                    Err(error) => {
                        self.settle(
                            recipe,
                            &task,
                            Err(PreparationFailure::Admission(Arc::new(error))),
                        );
                        return task.wait().await;
                    }
                }
            };
            let acquisition = if completed_original {
                OriginalAcquisition::LoadCompleted
            } else {
                OriginalAcquisition::CompileIfAbsent
            };
            #[cfg(test)]
            let fresh_launch_observer = if compiler_work.is_some() {
                self.fresh_launch_observer.lock().take()
            } else {
                None
            };
            let task = Arc::clone(&task);
            let owner = Arc::clone(self);
            let key = recipe.clone();
            // This task belongs to the preparation owner, independently of any
            // actor waiting for it. Dropping a waiter cannot interrupt its peers.
            tokio::spawn(async move {
                let outcome = tidepool_runtime::spawn_blocking_in_span(move || {
                    if let Some((ticket, cancellation)) = compiler_work {
                        ticket.run_for_workload(workload, cancellation, || {
                            #[cfg(test)]
                            if let Some(observer) = fresh_launch_observer {
                                observer();
                            }
                            compile_installer(recipe, resolved, source, registry, acquisition)
                        })
                    } else {
                        compile_installer(recipe, resolved, source, registry, acquisition)
                    }
                })
                .await
                .unwrap_or_else(|error| Err(PreparationFailure::Native(error.to_string())));
                owner.settle(key, &task, outcome);
            });
        }
        task.wait().await
    }

    fn settle(
        &self,
        key: InstallerRecipe,
        task: &Arc<PreparationTask>,
        outcome: Result<Arc<PreparedToolset>, PreparationFailure>,
    ) {
        let succeeded = outcome.is_ok();
        let mut state = self.state.lock();
        // A waiter can observe completion while this lock is held, but its next
        // lookup cannot pass the failed-task retirement or ready-cache bound.
        *task.outcome.lock() = Some(outcome);
        if !state
            .tasks
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, task))
        {
            drop(state);
            task.completed.notify_waiters();
            return;
        }
        if !succeeded {
            state.tasks.remove(&key);
            drop(state);
            task.completed.notify_waiters();
            return;
        }
        state.ready_order.push_back(key);
        while state.ready_order.len() > RETAINED_TOOLSETS {
            if let Some(retired) = state.ready_order.pop_front() {
                state.tasks.remove(&retired);
            }
        }
        drop(state);
        task.completed.notify_waiters();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn recipe(entry: &str) -> InstallerRecipe {
        InstallerRecipe {
            source_authority: [0; 32],
            source_revision: "frozen source".into(),
            roots: Vec::new(),
            preamble: String::new(),
            imports: String::new(),
            entry: entry.into(),
            effects: Vec::new(),
        }
    }

    /// The consumer supplies a genuinely issued entry; this exercises cache
    /// retirement without manufacturing source/native authority for a fixture.
    pub(crate) fn ready_bound_preserves_installed_lease(ready: Arc<PreparedToolset>) {
        let owner = ToolsetPreparation::default();
        let first_key = recipe("oldest");
        let (first, launch) = owner.lookup(&first_key);
        assert!(launch);
        let retired = Arc::downgrade(&first);
        owner.settle(first_key.clone(), &first, Ok(Arc::clone(&ready)));
        drop(first);
        for index in 0..RETAINED_TOOLSETS {
            let key = recipe(&format!("ready-{index}"));
            let (task, launch) = owner.lookup(&key);
            assert!(launch);
            owner.settle(key, &task, Ok(Arc::clone(&ready)));
        }
        assert_eq!(owner.state.lock().ready_order.len(), RETAINED_TOOLSETS);
        assert_eq!(owner.state.lock().tasks.len(), RETAINED_TOOLSETS);
        assert!(
            retired.upgrade().is_none(),
            "the seventeenth ready entry retires the oldest lookup custody"
        );
        let retained = Arc::downgrade(&ready);
        drop(owner);
        assert!(
            retained.upgrade().is_some(),
            "installed tool leases retain the original entry after cache retirement"
        );
        assert!(ready.entry.compiled().original_compile_input().is_some());
    }

    #[test]
    fn failed_preparation_retires_only_its_exact_lookup_task() {
        let owner = ToolsetPreparation::default();
        let key = recipe("failed");
        let (failed, launched) = owner.lookup(&key);
        assert!(launched);
        owner.settle(
            key.clone(),
            &failed,
            Err(PreparationFailure::Source("transient".into())),
        );
        assert!(!owner.state.lock().tasks.contains_key(&key));
        let (retry, launched) = owner.lookup(&key);
        assert!(launched);
        owner.settle(
            key.clone(),
            &failed,
            Err(PreparationFailure::Source("late original refusal".into())),
        );
        assert!(Arc::ptr_eq(
            owner.state.lock().tasks.get(&key).unwrap(),
            &retry
        ));
        assert!(
            failed.outcome.lock().is_some(),
            "admitted waiters retain their settled refusal"
        );
    }

    #[tokio::test]
    async fn dropping_one_waiter_preserves_the_other_waiter_and_completion() {
        let task = Arc::new(PreparationTask {
            outcome: Mutex::new(None),
            completed: tokio::sync::Notify::new(),
        });
        let cancelled_task = Arc::clone(&task);
        let cancelled = tokio::spawn(async move { cancelled_task.wait().await });
        let remaining_task = Arc::clone(&task);
        let remaining = tokio::spawn(async move { remaining_task.wait().await });
        tokio::task::yield_now().await;
        cancelled.abort();
        *task.outcome.lock() = Some(Err(PreparationFailure::Source("original refusal".into())));
        task.completed.notify_waiters();
        assert!(
            matches!(remaining.await.unwrap(), Err(PreparationFailure::Source(detail)) if detail == "original refusal")
        );
        assert!(
            matches!(task.wait().await, Err(PreparationFailure::Source(detail)) if detail == "original refusal")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observed_failure_has_already_retired_before_immediate_retry() {
        let owner = Arc::new(ToolsetPreparation::default());
        let key = recipe("retry");
        let (failed, launch) = owner.lookup(&key);
        assert!(launch);
        let waiter_owner = Arc::clone(&owner);
        let waiter_key = key.clone();
        let waiter_task = Arc::clone(&failed);
        let waiter = tokio::spawn(async move {
            assert!(waiter_task.wait().await.is_err());
            let (retry, launch) = waiter_owner.lookup(&waiter_key);
            assert!(launch);
            assert!(!Arc::ptr_eq(&retry, &waiter_task));
        });
        owner.settle(
            key,
            &failed,
            Err(PreparationFailure::Source("transient".into())),
        );
        waiter.await.unwrap();
    }

    #[test]
    fn only_an_absent_original_path_can_select_compilation() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("entry");
        assert!(!entry_path_present(&output).unwrap());
        std::os::unix::fs::symlink(root.path().join("missing"), &output).unwrap();
        assert!(entry_path_present(&output).is_err());
        std::fs::remove_file(&output).unwrap();
        std::fs::write(&output, b"invalid original").unwrap();
        assert!(entry_path_present(&output).is_err());
        std::fs::remove_file(&output).unwrap();
        std::fs::create_dir(&output).unwrap();
        assert!(entry_path_present(&output).unwrap());
    }

    #[test]
    fn completed_acquisition_cannot_create_source_after_original_disappears() {
        let root = tempfile::tempdir().unwrap();
        let preparation = uuid::Uuid::new_v4();
        let recipe = recipe("gone");
        let storage = crate::SourceEntryStorage::FreshCompilation {
            directory: root.path().to_owned(),
            preparation,
        };
        let original = tidepool_atomic_write::DirectoryAnchor::open_existing(root.path())
            .unwrap()
            .child(durable_recipe_key(&recipe).unwrap())
            .unwrap()
            .child(preparation.to_string())
            .unwrap();
        let entry = original.child("entry").unwrap();
        assert!(entry_path_present(entry.path()).unwrap());
        std::fs::remove_dir(entry.path()).unwrap();
        let before = tidepool_extract_cmd::extract_spawn_count();
        let result = retained_installer(
            &recipe,
            &storage,
            "module RetainedMissing where\n__prepared = (1 :: Int)\n",
            OriginalAcquisition::LoadCompleted,
        );
        assert!(result.is_err());
        assert!(!original.path().join("RetainedMissing.hs").exists());
        assert!(!entry.path().exists());
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
    }
}

fn compile_installer(
    recipe: InstallerRecipe,
    resolved: ResolvedSpec,
    source: crate::CheckpointSourceLayer,
    registry: Arc<ImageRegistry>,
    acquisition: OriginalAcquisition,
) -> Result<Arc<PreparedToolset>, PreparationFailure> {
    let installation = super::installation_expression(&recipe.entry, &recipe.effects);
    let dispatcher_effects = installation.dispatcher_effect_row();
    let templates =
        resident_workbench_templates(&recipe.preamble, &dispatcher_effects, &recipe.imports);
    let (compiled, selection) = if let Some(storage) = source.prepared_entries() {
        let template = templates
            .iter()
            .find(|template| {
                template.kind == tidepool_runtime::session::TemplateSelector::BindDiscard
            })
            .ok_or_else(|| {
                PreparationFailure::Source("installer has no discarded-bind template".into())
            })?;
        let wrapper = tidepool_runtime::session::render_template(
            &template.source,
            &installation.expression,
            &[],
        );
        retained_installer(&recipe, storage, &wrapper, acquisition)?
    } else {
        (
            compile_unprepared_installer(&recipe, &templates, &installation.expression)?,
            None,
        )
    };
    let compiled: Arc<CompiledTurn> = Arc::new(compiled);
    let nominal_artifacts = nominal_artifacts(&compiled, &recipe.effects)?;
    let entry = PreparedSourceEntry::prepare(compiled, registry)
        .map_err(|error| PreparationFailure::Native(error.to_string()))?;
    Ok(Arc::new(PreparedToolset {
        entry,
        entry_name: recipe.entry,
        resolved,
        source_revision: recipe.source_revision,
        completed_selection: selection.map(|selection| (selection.recipe, selection.original)),
        _source: source,
        _nominal_artifacts: nominal_artifacts,
    }))
}

fn durable_recipe_key(recipe: &InstallerRecipe) -> Result<String, PreparationFailure> {
    Ok(blake3::hash(
        &serde_json::to_vec(recipe)
            .map_err(|error| PreparationFailure::Source(error.to_string()))?,
    )
    .to_hex()
    .to_string())
}

/// Presence only chooses whether a compiler ticket is needed. It never grants
/// original custody: all present outputs still pass the complete loader.
fn selected_original_present(
    recipe: &InstallerRecipe,
    source: &crate::CheckpointSourceLayer,
) -> Result<bool, PreparationFailure> {
    match source.prepared_entries() {
        Some(crate::SourceEntryStorage::CompletedOriginal { .. }) => Ok(true),
        Some(crate::SourceEntryStorage::FreshCompilation {
            directory,
            preparation,
        }) => entry_path_present(
            &directory
                .join(durable_recipe_key(recipe)?)
                .join(preparation.to_string())
                .join("entry"),
        ),
        None => Ok(false),
    }
}

fn entry_path_present(path: &std::path::Path) -> Result<bool, PreparationFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(PreparationFailure::Source(format!(
            "completed installer path is not an original directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PreparationFailure::Source(error.to_string())),
    }
}

/// This explicit developer/fixture path has no retained source storage.
fn compile_unprepared_installer(
    recipe: &InstallerRecipe,
    templates: &[tidepool_runtime::session::TurnTemplate],
    expression: &str,
) -> Result<CompiledTurn, PreparationFailure> {
    let scratch =
        tempfile::tempdir().map_err(|error| PreparationFailure::Source(error.to_string()))?;
    let include = recipe
        .roots
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let result = run_turn(TurnRequest {
        exact_context: None,
        session_id: None,
        turn_text: expression,
        templates,
        include: &include,
        session_root: scratch.path(),
        inject_modules: &[],
        gen: 1,
        verdict: Some(TurnClassification {
            kind: TurnKind::Bind,
            binders: Vec::new(),
            items: Vec::new(),
        }),
        target: None,
        retained_imports: &[],
    })
    .map_err(|failure| {
        let mut diagnostic = tidepool_runtime::classify_compile(&failure.error);
        diagnostic.message = tidepool_runtime::session::render_turn_compile_error(
            &failure.error,
            failure.attempted_source.as_deref(),
            expression,
            "<agent-spec-installation>",
        );
        PreparationFailure::Compiler(diagnostic)
    })?;
    let TurnResult::Bind {
        bound, compiled, ..
    } = result
    else {
        return Err(PreparationFailure::Source(
            "source installer is not a discarded bind".into(),
        ));
    };
    if !bound.is_empty() {
        return Err(PreparationFailure::Source(
            "source installer exports public notebook bindings".into(),
        ));
    }
    Ok(compiled)
}

fn retained_installer(
    recipe: &InstallerRecipe,
    storage: &crate::SourceEntryStorage,
    wrapper: &str,
    acquisition: OriginalAcquisition,
) -> Result<(CompiledTurn, Option<CompletedInstallerSelection>), PreparationFailure> {
    use tidepool_toolchain::artifacts::{
        load_selected_production_entry, prepare_frozen_production_entry, FrozenEntrySources,
        ProductionEntrySources,
    };
    use tidepool_toolchain::toolchain::CompilerDeploymentConfiguration;
    let source_error =
        |error: &dyn std::fmt::Display| PreparationFailure::Source(error.to_string());
    let key = durable_recipe_key(recipe)?;
    let directory = storage.directory().join(&key);
    let (original, selected) = match storage {
        crate::SourceEntryStorage::FreshCompilation { preparation, .. } => {
            let root = tidepool_atomic_write::DirectoryAnchor::open_existing(storage.directory())
                .map_err(|error| source_error(&error))?;
            let selected = *preparation;
            let original = root
                .child(&key)
                .and_then(|root| root.child(selected.to_string()))
                .map_err(|error| source_error(&error))?;
            (original.path().to_owned(), selected)
        }
        crate::SourceEntryStorage::CompletedOriginal { selections, .. } => {
            let selected =
                selections
                    .get(&key)
                    .ok_or_else(|| PreparationFailure::AbsentSelection {
                        recipe: key.clone(),
                    })?;
            let original = directory.join(selected.to_string());
            (original, *selected)
        }
    };
    let canonical_root =
        std::fs::canonicalize(storage.directory()).map_err(|error| source_error(&error))?;
    if std::fs::canonicalize(&original).map_err(|error| source_error(&error))?
        != canonical_root.join(&key).join(selected.to_string())
    {
        return Err(PreparationFailure::Source(
            "completed installer escapes its retained source owner".into(),
        ));
    }
    let module = tidepool_toolchain::extract_module_name(wrapper)
        .ok_or_else(|| PreparationFailure::Source("installer wrapper has no module".into()))?;
    let source_path = original.join(format!("{module}.hs"));
    let output = original.join("entry");
    let completed = entry_path_present(&output)?;
    if !completed && matches!(acquisition, OriginalAcquisition::LoadCompleted) {
        return Err(PreparationFailure::Source(
            "selected completed original disappeared before acquisition".into(),
        ));
    }
    if !completed && matches!(storage, crate::SourceEntryStorage::FreshCompilation { .. }) {
        match std::fs::symlink_metadata(&source_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if std::fs::read(&source_path).map_err(|error| source_error(&error))?
                    != wrapper.as_bytes()
                {
                    return Err(PreparationFailure::Source(
                        "retained installer wrapper changed before retry".into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tidepool_atomic_write::write_durable(&source_path, wrapper.as_bytes())
                    .map_err(|error| source_error(&error))?;
            }
            Ok(_) => {
                return Err(PreparationFailure::Source(
                    "retained installer wrapper is an alias or special file".into(),
                ))
            }
            Err(error) => return Err(source_error(&error)),
        }
    } else {
        let metadata =
            std::fs::symlink_metadata(&source_path).map_err(|error| source_error(&error))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || std::fs::read(&source_path).map_err(|error| source_error(&error))?
                != wrapper.as_bytes()
        {
            return Err(PreparationFailure::Source(
                "completed installer wrapper differs from selected source and row".into(),
            ));
        }
    }
    let sources = FrozenEntrySources::capture(&recipe.roots, &source_path)
        .map_err(|error| source_error(&error))?;
    if sources
        .source_revision(b"exomonad-agent-spec-ordered-source-closure-v1")
        .map_err(|error| source_error(&error))?
        != recipe.source_revision
    {
        return Err(PreparationFailure::Source(
            "source owner snapshot changed before installer acquisition".into(),
        ));
    }
    if !completed && matches!(storage, crate::SourceEntryStorage::FreshCompilation { .. }) {
        prepare_frozen_production_entry(&sources, &original, &output).map_err(|error| {
            PreparationFailure::Compiler(tidepool_runtime::classify_compile(&error))
        })?;
    }
    let CompilerDeploymentConfiguration::Configured(authority) =
        CompilerDeploymentConfiguration::from_env().map_err(|error| source_error(&error))?
    else {
        return Err(PreparationFailure::Source(
            "completed installer requires configured compiler deployment".into(),
        ));
    };
    let loaded = load_selected_production_entry(
        &output,
        &authority,
        &ProductionEntrySources::FrozenWorkspace(sources),
    )
    .map_err(|error| source_error(&error))?;
    // A previous rename may have succeeded before parent fsync failed. This
    // confirms publication of that exact validated original without source.
    tidepool_atomic_write::sync_parent_directory(&output).map_err(|error| source_error(&error))?;
    let compiled =
        CompiledTurn::from_production_entry(&loaded).map_err(|error| source_error(&error))?;
    Ok((
        compiled,
        Some(CompletedInstallerSelection {
            recipe: key,
            original: selected,
        }),
    ))
}

struct CompletedInstallerSelection {
    recipe: String,
    original: uuid::Uuid,
}
