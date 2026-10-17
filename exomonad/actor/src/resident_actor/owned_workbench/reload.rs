//! Staged configuration publication for the admitted Reload effect.
use super::*;
use crate::generated::source::SourceError;
use crate::resident_workbench::{ActorGeneration, ActorGenerationCommit, ActorRequestSnapshot};
use crate::HasSourceInspection;
use tidepool_bridge_effects::{
    RlPolicy, RlRefusal, RlRefusalCategory, RlSurfaceDiff, SrReloadOutcome,
};

pub(crate) struct CoherentReloadOperation<H, O> {
    workbench: crate::ResidentActorWorkbench<H, O>,
    layers: crate::ActorSourceLayerResolver,
    descriptor: crate::ActorDescriptor,
    state: Arc<crate::resident_workbench::InstalledToolsState>,
    expected: Arc<ActorGeneration>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    checked: Vec<String>,
    policy: RlPolicy,
    lifecycle: Option<crate::LifecycleHandle>,
}

fn unreadable(error: impl std::fmt::Display) -> SourceError {
    SourceError::SourceUnreadable(error.to_string())
}

impl<H, O> CoherentReloadOperation<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn run(self) -> Result<SrReloadOutcome, SourceError> {
        let active = self
            .layers
            .clone()
            .inspection_services()
            .source_inspection()
            .revision(self.expected.installation.source())
            .map_err(|error| match error {
                crate::SourceInspectionError::Unavailable(detail) => {
                    SourceError::SourceUnavailable(detail)
                }
                crate::SourceInspectionError::Unreadable(detail) => {
                    SourceError::SourceUnreadable(detail)
                }
            })?;
        if self
            .control
            .as_ref()
            .is_some_and(|control| control.cancellation_requested())
        {
            return Ok(SrReloadOutcome::ReloadCancelled(active));
        }
        let requested_checks = !self.checked.is_empty();
        let layers = self.layers.clone();
        let actor = self.expected.context.actor;
        let source = self.expected.installation.source().clone();
        let staged = tokio::task::spawn_blocking(move || {
            layers.stage_reload(actor.into(), &source, &self.checked)
        })
        .await
        .map_err(unreadable)?
        .map_err(|outcome| match outcome {
            crate::SourceLayerReload::Unavailable(detail) => SourceError::SourceUnavailable(detail),
            other => unreadable(format!("candidate capture refused: {other:?}")),
        })?;
        let previous = staged.previous_revision().clone();
        let candidate_revision = staged.candidate_revision().clone();
        let configuration_unchanged =
            staged.configuration_digest() == self.expected.configuration_digest;
        if configuration_unchanged && !requested_checks {
            return Ok(SrReloadOutcome::ReloadUnchanged(previous));
        }
        let (context, authority) = WorkbenchCompilationAuthority::admit(
            self.expected.context.clone(),
            staged.source().clone(),
            None,
            Some(&self.layers),
        )
        .map_err(unreadable)?;
        let workbench = self.workbench.with_compilation_authority(authority);
        let install = self
            .expected
            .installation
            .tools()
            .ok_or_else(|| {
                SourceError::SourceUnavailable(
                    "actor has no source-defined tool installation".into(),
                )
            })?
            .install
            .checked_add(1)
            .ok_or_else(|| {
                SourceError::SourceUnavailable("tool installation counter exhausted".into())
            })?;
        let active_surface = self
            .expected
            .installation
            .tools()
            .expect("selected source installation")
            .declaration_snapshot();
        let candidate = match workbench
            .prepare_candidate_tools(
                context.clone(),
                install,
                self.descriptor.capabilities().effect_keys().to_vec(),
                staged.validation_imports().clone(),
            )
            .await
        {
            Ok(candidate) => candidate,
            Err(ResidentActorWorkbenchError::InvocationCancelled) => {
                return Ok(SrReloadOutcome::ReloadCancelled(previous));
            }
            Err(ResidentActorWorkbenchError::UnsupportedToolSurface { detail, surface }) => {
                let changes = exomonad_tool::surface::compare_surfaces(&active_surface, &surface);
                return Ok(SrReloadOutcome::ReloadRefused(
                    previous,
                    candidate_revision,
                    RlRefusal {
                        category: RlRefusalCategory::UnsupportedSurface,
                        detail,
                        surface: surface_diff(&changes),
                    },
                ));
            }
            Err(error)
                if error.failure_diagnostic().is_some_and(|diagnostic| {
                    diagnostic.class == tidepool_toolchain::failclass::FailureClass::UserHaskell
                }) =>
            {
                return Ok(SrReloadOutcome::ReloadRejected(
                    previous,
                    candidate_revision,
                    error.to_string(),
                ));
            }
            Err(error) => return Err(unreadable(error)),
        };
        let changes = exomonad_tool::surface::compare_surfaces(
            &active_surface,
            &candidate.declaration_snapshot(),
        );
        let surface = surface_diff(&changes);
        if !changes.is_empty() && self.policy == RlPolicy::CompatibleOnly {
            return Ok(SrReloadOutcome::ReloadRefused(
                previous,
                candidate_revision,
                RlRefusal {
                    category: RlRefusalCategory::SurfaceIncompatible,
                    detail: exomonad_tool::surface::describe_changes(&changes),
                    surface,
                },
            ));
        }
        if configuration_unchanged {
            return Ok(SrReloadOutcome::ReloadUnchanged(previous));
        }
        let installation = crate::InstalledToolLease::new(
            actor,
            staged.source().clone(),
            Some(Arc::new(candidate)),
        );
        let prompts = staged.prompt_snapshot().clone();
        let references = installation
            .generation_references(staged.source_reference().clone(), prompts.clone())
            .map_err(unreadable)?;
        let prepared = workbench
            .prepare_actor_generation(
                context.clone(),
                self.descriptor.clone(),
                Some(self.expected.clone()),
                references,
                installation.clone(),
            )
            .await
            .map_err(unreadable)?;
        let publication = self.control.as_ref().map_or_else(
            tidepool_runtime::session::PublicationDecision::new,
            |control| control.publication_decision(),
        );
        let result = workbench
            .commit_actor_generation(
                context,
                self.descriptor,
                self.state,
                ActorRequestSnapshot::Generation(self.expected),
                prepared,
                installation,
                prompts,
                staged.configuration_digest(),
                publication,
                if self.control.is_some() {
                    crate::resident_workbench::ActorGenerationPublication::Continuing
                } else {
                    crate::resident_workbench::ActorGenerationPublication::Terminal
                },
            )
            .await
            .map_err(unreadable)?;
        match result {
            ActorGenerationCommit::Published {
                generation,
                cancellation_pending,
            } => {
                if let Some(control) = self.control.as_ref() {
                    control.complete_continuing_publication(cancellation_pending);
                }
                if let Some(lifecycle) = self.lifecycle {
                    lifecycle.generation_published(generation);
                }
                Ok(SrReloadOutcome::ReloadPublished(
                    previous,
                    candidate_revision,
                    staged.changed_modules().to_vec(),
                    surface,
                ))
            }
            ActorGenerationCommit::Refused(_detail)
                if self
                    .control
                    .as_ref()
                    .is_some_and(|control| control.cancellation_requested()) =>
            {
                Ok(SrReloadOutcome::ReloadCancelled(previous))
            }
            ActorGenerationCommit::Refused(detail) => Err(SourceError::SourceUnavailable(detail)),
            ActorGenerationCommit::Unconfirmed(detail) => Ok(
                SrReloadOutcome::ReloadPublicationUnconfirmed(candidate_revision, detail),
            ),
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) fn coherent_reload_operation(
        &self,
        generation: Arc<ActorGeneration>,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        checked: Vec<String>,
        policy: RlPolicy,
    ) -> Result<CoherentReloadOperation<H, O>, SourceError> {
        let active = self.installed_tools.current_generation().ok_or_else(|| {
            SourceError::SourceUnavailable("actor generation admission is unavailable".into())
        })?;
        if !Arc::ptr_eq(&active, &generation) {
            return Err(SourceError::SourceUnavailable("issued generation is no longer current; an already accepted computation remains pinned".into()));
        }
        let tools = generation.installation.tools().ok_or_else(|| {
            SourceError::SourceUnavailable("actor has no source-defined tool installation".into())
        })?;
        if tools.resolved_spec().is_none() {
            return Err(SourceError::SourceUnavailable(
                "explicit live AgentSpec implementations have no editable source-backed reload"
                    .into(),
            ));
        }
        Ok(CoherentReloadOperation {
            workbench: self.environment.application_workbench(),
            layers: self.environment.source_layers.clone().ok_or_else(|| {
                SourceError::SourceUnavailable("actor has no editable source owner".into())
            })?,
            descriptor: self.descriptor.clone(),
            state: self.installed_tools.clone(),
            expected: generation,
            control,
            checked,
            policy,
            lifecycle: self.lifecycle.clone(),
        })
    }
}

fn surface_diff(changes: &[exomonad_tool::surface::SurfaceChange]) -> RlSurfaceDiff {
    use exomonad_tool::surface::SurfaceChange;
    let mut diff = RlSurfaceDiff {
        added: Vec::new(),
        removed: Vec::new(),
        changed: Vec::new(),
    };
    for change in changes {
        let (target, name) = match change {
            SurfaceChange::Added { name } => (&mut diff.added, name),
            SurfaceChange::Removed { name } => (&mut diff.removed, name),
            SurfaceChange::Changed { name, .. } | SurfaceChange::Moved { name, .. } => {
                (&mut diff.changed, name)
            }
        };
        if !target.contains(name) {
            target.push(name.clone());
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use exomonad_tool::surface::{SurfaceChange, ToolField};

    #[test]
    fn surface_receipt_separates_added_removed_and_changed_without_duplicate_names() {
        let diff = surface_diff(&[
            SurfaceChange::Removed {
                name: "apply_draft".into(),
            },
            SurfaceChange::Added {
                name: "activate_draft".into(),
            },
            SurfaceChange::Changed {
                name: "notebook".into(),
                fields: vec![ToolField::Description],
            },
            SurfaceChange::Moved {
                name: "notebook".into(),
                from: 0,
                to: 1,
            },
        ]);
        assert_eq!(diff.added, ["activate_draft"]);
        assert_eq!(diff.removed, ["apply_draft"]);
        assert_eq!(diff.changed, ["notebook"]);
    }
}
