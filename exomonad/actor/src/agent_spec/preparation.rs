//! Source-owned installer preparation; waiters do not own the shared task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, CompiledTurn, ImageRegistry, PreparedSourceEntry,
    TurnClassification, TurnKind, TurnRequest, TurnResult,
};

use super::ResolvedSpec;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstallerRecipe {
    pub(crate) source_revision: String,
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) preamble: String,
    pub(crate) imports: String,
    pub(crate) entry: String,
    pub(crate) effects: Vec<crate::ActorEffectKey>,
}

pub(crate) struct PreparedToolset {
    pub(crate) entry: PreparedSourceEntry,
    pub(crate) resolved: ResolvedSpec,
    pub(crate) source_revision: String,
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
            Ok((*first).clone())
        })
        .collect()
}

#[derive(Clone)]
pub(crate) enum PreparationFailure {
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
    tasks: Mutex<HashMap<InstallerRecipe, Arc<PreparationTask>>>,
}

impl ToolsetPreparation {
    pub(crate) async fn prepare(
        &self,
        recipe: InstallerRecipe,
        resolved: ResolvedSpec,
        source: crate::CheckpointSourceLayer,
        registry: Arc<ImageRegistry>,
    ) -> Result<Arc<PreparedToolset>, PreparationFailure> {
        let (task, launch) = {
            let mut tasks = self.tasks.lock();
            match tasks.get(&recipe) {
                Some(task) => (Arc::clone(task), false),
                None => {
                    let task = Arc::new(PreparationTask {
                        outcome: Mutex::new(None),
                        completed: tokio::sync::Notify::new(),
                    });
                    tasks.insert(recipe.clone(), Arc::clone(&task));
                    (task, true)
                }
            }
        };
        if launch {
            let task = Arc::clone(&task);
            // This task belongs to the preparation owner, independently of any
            // actor waiting for it. Dropping a waiter cannot interrupt its peers.
            tokio::spawn(async move {
                let outcome = tidepool_runtime::spawn_blocking_in_span(move || {
                    compile_installer(recipe, resolved, source, registry)
                })
                .await
                .unwrap_or_else(|error| Err(PreparationFailure::Native(error.to_string())));
                *task.outcome.lock() = Some(outcome);
                task.completed.notify_waiters();
            });
        }
        task.wait().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        turn_text: &installation.expression,
        templates: &templates,
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
            &installation.expression,
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
    let compiled: Arc<CompiledTurn> = Arc::new(compiled);
    let nominal_artifacts = nominal_artifacts(&compiled, &recipe.effects)?;
    let entry = PreparedSourceEntry::prepare(compiled, registry)
        .map_err(|error| PreparationFailure::Native(error.to_string()))?;
    Ok(Arc::new(PreparedToolset {
        entry,
        resolved,
        source_revision: recipe.source_revision,
        _source: source,
        _nominal_artifacts: nominal_artifacts,
    }))
}
