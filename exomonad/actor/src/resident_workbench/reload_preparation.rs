//! Coherent preparation under the existing source, compiler and machine owners.
use super::*;
use crate::generated::source::SourceError;
use crate::resident_actor::WorkbenchCompilationAuthority;
use crate::HasSourceInspection;
use tidepool_bridge_effects::{
    RlPolicy, RlRefusal, RlRefusalCategory, RlSurfaceDiff, SrReloadOutcome,
};

pub(crate) enum ReloadPreparationOutcome {
    Ready(PreparedActorGeneration),
    Settled(SrReloadOutcome),
}

/// Prepare exactly one issued generation without publishing its successor.
/// Source capture may block; dropping its waiter does not abort blocking IO.
/// A ready candidate retains its source/spec/surface/prompts and expected owner,
/// but publication must still recheck current authority and cancellation.
pub(crate) trait ReloadPreparation: Send + Sync {
    fn prepare(
        &self,
        checked: Vec<String>,
        policy: RlPolicy,
    ) -> futures_util::future::BoxFuture<'_, Result<ReloadPreparationOutcome, SourceError>>;
}

pub(crate) trait HasReloadPreparation {
    type ReloadPreparation: ReloadPreparation + ?Sized;
    fn reload_preparation(&self) -> &Self::ReloadPreparation;
}

/// Cheap service view; no source, machine or publication owner accessor.
#[derive(Clone)]
pub(crate) struct ReloadPreparationServices {
    preparation: Arc<dyn ReloadPreparation>,
}

impl HasReloadPreparation for ReloadPreparationServices {
    type ReloadPreparation = dyn ReloadPreparation;
    fn reload_preparation(&self) -> &Self::ReloadPreparation {
        self.preparation.as_ref()
    }
}

struct WorkbenchReloadPreparation<H, O> {
    runner: ResidentActorRunner<H, O>,
    layers: crate::ActorSourceLayerResolver,
    descriptor: crate::ActorDescriptor,
    expected: Arc<ActorGeneration>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
}

impl ReloadPreparationServices {
    pub(crate) fn new<H, O>(
        runner: ResidentActorRunner<H, O>,
        layers: crate::ActorSourceLayerResolver,
        descriptor: crate::ActorDescriptor,
        expected: Arc<ActorGeneration>,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> Self
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        Self {
            preparation: Arc::new(WorkbenchReloadPreparation {
                runner,
                layers,
                descriptor,
                expected,
                control,
            }),
        }
    }
}

fn unreadable(error: impl std::fmt::Display) -> SourceError {
    SourceError::SourceUnreadable(error.to_string())
}

impl<H, O> ReloadPreparation for WorkbenchReloadPreparation<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    fn prepare(
        &self,
        checked: Vec<String>,
        policy: RlPolicy,
    ) -> futures_util::future::BoxFuture<'_, Result<ReloadPreparationOutcome, SourceError>> {
        Box::pin(async move {
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
                return Ok(ReloadPreparationOutcome::Settled(
                    SrReloadOutcome::ReloadCancelled(active),
                ));
            }
            let requested_checks = !checked.is_empty();
            let layers = self.layers.clone();
            let actor = self.expected.context.actor;
            let source = self.expected.installation.source().clone();
            let staged = tokio::task::spawn_blocking(move || {
                layers.stage_reload(actor.into(), &source, &checked)
            })
            .await
            .map_err(unreadable)?
            .map_err(|outcome| match outcome {
                crate::SourceLayerReload::Unavailable(detail) => {
                    SourceError::SourceUnavailable(detail)
                }
                other => unreadable(format!("candidate capture refused: {other:?}")),
            })?;
            let previous = staged.previous_revision().clone();
            let candidate_revision = staged.candidate_revision().clone();
            let configuration_unchanged =
                staged.configuration_digest() == self.expected.configuration_digest;
            if configuration_unchanged && !requested_checks {
                return Ok(ReloadPreparationOutcome::Settled(
                    SrReloadOutcome::ReloadUnchanged(previous),
                ));
            }
            let (context, authority) = WorkbenchCompilationAuthority::admit(
                self.expected.context.clone(),
                staged.source().clone(),
                None,
                Some(&self.layers),
            )
            .map_err(unreadable)?;
            let workbench = self
                .runner
                .application_workbench()
                .with_compilation_authority(authority);
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
                    return Ok(ReloadPreparationOutcome::Settled(
                        SrReloadOutcome::ReloadCancelled(previous),
                    ));
                }
                Err(ResidentActorWorkbenchError::UnsupportedToolSurface { detail, surface }) => {
                    let changes =
                        exomonad_tool::surface::compare_surfaces(&active_surface, &surface);
                    return Ok(ReloadPreparationOutcome::Settled(
                        SrReloadOutcome::ReloadRefused(
                            previous,
                            candidate_revision,
                            RlRefusal {
                                category: RlRefusalCategory::UnsupportedSurface,
                                detail,
                                surface: surface_diff(&changes),
                            },
                        ),
                    ));
                }
                Err(error)
                    if error.failure_diagnostic().is_some_and(|diagnostic| {
                        diagnostic.class == tidepool_toolchain::failclass::FailureClass::UserHaskell
                    }) =>
                {
                    return Ok(ReloadPreparationOutcome::Settled(
                        SrReloadOutcome::ReloadRejected(
                            previous,
                            candidate_revision,
                            error.to_string(),
                        ),
                    ));
                }
                Err(error) => return Err(unreadable(error)),
            };
            let changes = exomonad_tool::surface::compare_surfaces(
                &active_surface,
                &candidate.declaration_snapshot(),
            );
            let surface = surface_diff(&changes);
            if !changes.is_empty() && policy == RlPolicy::CompatibleOnly {
                return Ok(ReloadPreparationOutcome::Settled(
                    SrReloadOutcome::ReloadRefused(
                        previous,
                        candidate_revision,
                        RlRefusal {
                            category: RlRefusalCategory::SurfaceIncompatible,
                            detail: exomonad_tool::surface::describe_changes(&changes),
                            surface,
                        },
                    ),
                ));
            }
            if configuration_unchanged {
                return Ok(ReloadPreparationOutcome::Settled(
                    SrReloadOutcome::ReloadUnchanged(previous),
                ));
            }
            let installation = crate::InstalledToolLease::new(
                actor,
                staged.source().clone(),
                Some(Arc::new(candidate)),
            );
            let prompts = staged.prompt_snapshot().clone();
            let mut prepared = workbench
                .prepare_actor_generation(
                    context.clone(),
                    self.descriptor.clone(),
                    ActorRequestSnapshot::Generation(self.expected.clone()),
                    installation,
                    prompts,
                )
                .await
                .map_err(unreadable)?;
            prepared.reload_report = Some(PreparedReloadReport {
                previous,
                candidate: candidate_revision,
                changed_modules: staged.changed_modules().to_vec(),
                surface,
            });
            Ok(ReloadPreparationOutcome::Ready(prepared))
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
