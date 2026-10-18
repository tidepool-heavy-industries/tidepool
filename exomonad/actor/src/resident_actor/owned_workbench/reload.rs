//! Staged configuration publication for the admitted Reload effect.
use super::*;
use crate::generated::source::SourceError;
use crate::resident_workbench::{ActorGeneration, ActorGenerationCommit};
use tidepool_bridge_effects::{RlPolicy, SrReloadOutcome};

pub(crate) struct CoherentReloadOperation<H, O> {
    preparation: crate::resident_workbench::ReloadPreparationServices,
    publisher: crate::resident_workbench::ActorGenerationPublisher<H, O>,
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
        use crate::resident_workbench::{HasReloadPreparation, ReloadPreparationOutcome};
        async fn prepare<C: HasReloadPreparation>(
            services: &C,
            checked: Vec<String>,
            policy: RlPolicy,
        ) -> Result<ReloadPreparationOutcome, SourceError> {
            crate::resident_workbench::ReloadPreparation::prepare(
                services.reload_preparation(),
                checked,
                policy,
            )
            .await
        }
        let prepared = match prepare(&self.preparation, self.checked, self.policy).await? {
            ReloadPreparationOutcome::Settled(outcome) => return Ok(outcome),
            ReloadPreparationOutcome::Ready(prepared) => prepared,
        };
        let report = prepared
            .reload_report()
            .expect("reload preparation carries its report")
            .clone();
        let publication = self.control.as_ref().map_or_else(
            tidepool_runtime::session::PublicationDecision::new,
            |control| control.publication_decision(),
        );
        let result = self
            .publisher
            .publish(
                prepared,
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
                    report.previous,
                    report.candidate,
                    report.changed_modules,
                    report.surface,
                ))
            }
            ActorGenerationCommit::Refused(_detail)
                if self
                    .control
                    .as_ref()
                    .is_some_and(|control| control.cancellation_requested()) =>
            {
                Ok(SrReloadOutcome::ReloadCancelled(report.previous))
            }
            ActorGenerationCommit::Refused(detail) => Err(SourceError::SourceUnavailable(detail)),
            ActorGenerationCommit::Unconfirmed(detail) => Ok(
                SrReloadOutcome::ReloadPublicationUnconfirmed(report.candidate, detail),
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
        let tools = generation.installation().tools().ok_or_else(|| {
            SourceError::SourceUnavailable("actor has no source-defined tool installation".into())
        })?;
        if tools.resolved_spec().is_none() {
            return Err(SourceError::SourceUnavailable(
                "explicit live AgentSpec implementations have no editable source-backed reload"
                    .into(),
            ));
        }
        Ok(CoherentReloadOperation {
            preparation: crate::resident_workbench::ReloadPreparationServices::new(
                self.environment.application_workbench(),
                self.environment.source_layers.clone().ok_or_else(|| {
                    SourceError::SourceUnavailable("actor has no editable source owner".into())
                })?,
                self.descriptor.clone(),
                generation,
                control.clone(),
            ),
            publisher: self
                .environment
                .application_workbench()
                .generation_publisher(self.installed_tools.clone()),
            control,
            checked,
            policy,
            lifecycle: self.lifecycle.clone(),
        })
    }
}
