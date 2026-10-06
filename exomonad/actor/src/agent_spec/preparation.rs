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

#[derive(Clone)]
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
}

#[derive(Default)]
struct PreparationState {
    tasks: HashMap<InstallerRecipe, Arc<PreparationTask>>,
    ready_order: VecDeque<InstallerRecipe>,
}

const RETAINED_TOOLSETS: usize = 16;

impl ToolsetPreparation {
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
            let completed_original = matches!(
                source.prepared_entries(),
                Some(crate::SourceEntryStorage::CompletedOriginal { .. })
            );
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
            let task = Arc::clone(&task);
            let owner = Arc::clone(self);
            let key = recipe.clone();
            // This task belongs to the preparation owner, independently of any
            // actor waiting for it. Dropping a waiter cannot interrupt its peers.
            tokio::spawn(async move {
                let outcome = tidepool_runtime::spawn_blocking_in_span(move || {
                    if let Some((ticket, cancellation)) = compiler_work {
                        ticket.run_for_workload(workload, cancellation, || {
                            compile_installer(recipe, resolved, source, registry)
                        })
                    } else {
                        compile_installer(recipe, resolved, source, registry)
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
}

fn compile_installer(
    recipe: InstallerRecipe,
    resolved: ResolvedSpec,
    source: crate::CheckpointSourceLayer,
    registry: Arc<ImageRegistry>,
) -> Result<Arc<PreparedToolset>, PreparationFailure> {
    let installation = super::installation_expression(&recipe.entry, &recipe.effects);
    let dispatcher_effects = format!(
        "(Tidepool.Effects.Core.AgentTools ': Tidepool.Agent.Contract.SyncEffects {})",
        installation.effect_row,
    );
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
        retained_installer(&recipe, storage, &wrapper)?
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
    if let Some(selection) = selection.as_ref() {
        if let Some(path) = selection.publish.as_ref() {
            tidepool_atomic_write::write_durable(path, selection.original.to_string().as_bytes())
                .map_err(|error| PreparationFailure::Source(error.to_string()))?;
        }
    }
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
) -> Result<(CompiledTurn, Option<CompletedInstallerSelection>), PreparationFailure> {
    use tidepool_toolchain::artifacts::{
        load_selected_production_entry, prepare_frozen_production_entry, FrozenEntrySources,
        ProductionEntrySources,
    };
    use tidepool_toolchain::toolchain::CompilerDeploymentConfiguration;
    let source_error =
        |error: &dyn std::fmt::Display| PreparationFailure::Source(error.to_string());
    let key = blake3::hash(&serde_json::to_vec(recipe).map_err(|error| source_error(&error))?)
        .to_hex()
        .to_string();
    let directory = storage.directory().join(&key);
    let (original, selected, publication) = match storage {
        crate::SourceEntryStorage::FreshCompilation { .. } => {
            let root = tidepool_atomic_write::DirectoryAnchor::open_existing(storage.directory())
                .map_err(|error| source_error(&error))?;
            let selected = uuid::Uuid::new_v4();
            let original = root
                .child(&key)
                .and_then(|root| root.child(selected.to_string()))
                .map_err(|error| source_error(&error))?;
            (
                original.path().to_owned(),
                selected,
                Some(directory.join("selected")),
            )
        }
        crate::SourceEntryStorage::CompletedOriginal { selections, .. } => {
            let selected =
                selections
                    .get(&key)
                    .ok_or_else(|| PreparationFailure::AbsentSelection {
                        recipe: key.clone(),
                    })?;
            let bytes =
                std::fs::read(directory.join("selected")).map_err(|error| source_error(&error))?;
            if bytes != selected.to_string().as_bytes() {
                return Err(PreparationFailure::Source(
                    "completed installer pointer differs from owner-selected original".into(),
                ));
            }
            let original = directory.join(selected.to_string());
            let canonical_root =
                std::fs::canonicalize(storage.directory()).map_err(|error| source_error(&error))?;
            if std::fs::canonicalize(&original).map_err(|error| source_error(&error))?
                != canonical_root.join(&key).join(selected.to_string())
            {
                return Err(PreparationFailure::Source(
                    "completed installer escapes its retained source owner".into(),
                ));
            }
            (original, *selected, None)
        }
    };
    let module = tidepool_toolchain::extract_module_name(wrapper)
        .ok_or_else(|| PreparationFailure::Source("installer wrapper has no module".into()))?;
    let source_path = original.join(format!("{module}.hs"));
    let output = original.join("entry");
    if matches!(storage, crate::SourceEntryStorage::FreshCompilation { .. }) {
        tidepool_atomic_write::write_durable(&source_path, wrapper.as_bytes())
            .map_err(|error| source_error(&error))?;
    } else if std::fs::read(&source_path).map_err(|error| source_error(&error))?
        != wrapper.as_bytes()
    {
        return Err(PreparationFailure::Source(
            "completed installer wrapper differs from selected source and row".into(),
        ));
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
    if matches!(storage, crate::SourceEntryStorage::FreshCompilation { .. }) {
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
    let compiled =
        CompiledTurn::from_production_entry(&loaded).map_err(|error| source_error(&error))?;
    Ok((
        compiled,
        Some(CompletedInstallerSelection {
            recipe: key,
            original: selected,
            publish: publication,
        }),
    ))
}

struct CompletedInstallerSelection {
    recipe: String,
    original: uuid::Uuid,
    publish: Option<PathBuf>,
}
