//! Staged configuration publication for the admitted Reload effect.
use super::*;
use crate::generated::source::SourceError;
use crate::resident_workbench::ActorGeneration;
use tidepool_bridge_effects::{RlPolicy, SrReloadOutcome};

pub(crate) struct CoherentReloadOperation<H, O> {
    preparation: crate::resident_workbench::ReloadPreparationServices,
    publisher: crate::resident_workbench::ActorGenerationPublisher<H, O>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    checked: Vec<String>,
    policy: RlPolicy,
    kernel: KernelContext,
    inventory: ResidentGenerationInventory,
    diagnostic: WorkbenchOperationDiagnosticOwner,
}

impl<H, O> CoherentReloadOperation<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn run(
        self,
        checked_return: Option<tidepool_runtime::session::RuntimeCheckedReturnOwner>,
    ) -> Result<
        (
            SrReloadOutcome,
            Option<Arc<crate::local_actor::run_control::CommittedGenerationSupersession>>,
        ),
        SourceError,
    > {
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
            ReloadPreparationOutcome::Settled(outcome) => return Ok((outcome, None)),
            ReloadPreparationOutcome::Ready(prepared) => prepared,
        };
        let publication = self.control.as_ref().map_or_else(
            tidepool_runtime::session::PublicationDecision::new,
            |control| control.publication_decision(),
        );
        let preparation = self.publisher.into_publication_preparation(
            prepared,
            publication,
            if self.control.is_some() {
                crate::resident_workbench::ActorGenerationPublication::Continuing
            } else {
                crate::resident_workbench::ActorGenerationPublication::Terminal
            },
        );
        let receipt = self
            .kernel
            .retire_actor_generation(
                preparation,
                self.inventory,
                self.diagnostic,
                false,
                self.control,
                checked_return,
            )
            .await
            .map_err(|error| SourceError::SourceUnavailable(error.to_string()))?;
        Ok((
            SrReloadOutcome::ReloadRetirement(Arc::unwrap_or_clone(receipt.receipt)),
            receipt.supersession,
        ))
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) fn coherent_reload_operation(
        &self,
        kernel: KernelContext,
        coordinator: Arc<InvocationWork>,
        generation: Arc<ActorGeneration>,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        checked: Vec<String>,
        policy: RlPolicy,
        diagnostic: WorkbenchOperationDiagnosticOwner,
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
        let lifecycle = self.lifecycle.clone().ok_or_else(|| {
            SourceError::SourceUnavailable(
                "generation publication has no actor lifecycle owner".into(),
            )
        })?;
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
                .generation_publisher(
                    self.installed_tools.clone(),
                    crate::resident_workbench::ActorPublicationOwner::Actor(lifecycle),
                ),
            control,
            checked,
            policy,
            kernel,
            inventory: self.environment.generation_inventory(Some(coordinator)),
            diagnostic,
        })
    }
}
