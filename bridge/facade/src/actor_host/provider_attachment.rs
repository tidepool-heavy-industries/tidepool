//! The provider owner retains the Forest-issued attachment admission across
//! asynchronous setup. Forest owns persistence readiness and live placement.

use super::{CapturedOutput, ExomonadHandlerStack, ResidentForest};
use exomonad_actor::{ActorProviderAdmission, ActorRef, ResidentActorWorkbenchError};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct ProviderAttachment {
    forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    admission: Arc<ActorProviderAdmission>,
}

impl ProviderAttachment {
    pub(super) async fn admit(
        forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
        actor: ActorRef,
    ) -> Result<Self, ResidentActorWorkbenchError> {
        let admission = forest.authorize_provider_attachment(actor).await?;
        Ok(Self { forest, admission })
    }

    pub(super) fn actor(&self) -> ActorRef {
        self.admission.actor()
    }

    pub(super) fn native_readiness(
        &self,
    ) -> Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>> {
        self.admission.native_readiness()
    }

    pub(super) async fn initial_context_wire_observation(
        &self,
    ) -> Result<tidepool_bridge_effects::InitialContextObservation, ResidentActorWorkbenchError>
    {
        self.validate().await?;
        self.forest
            .initial_context_wire_observation(self.actor())
            .await
    }

    pub(super) async fn validate(&self) -> Result<(), ResidentActorWorkbenchError> {
        self.forest
            .validate_provider_attachment(&self.admission)
            .await
    }

    pub(super) async fn prepare_initial_context(
        &self,
    ) -> Result<exomonad_actor::InitialContextObservation, ResidentActorWorkbenchError> {
        self.validate().await?;
        self.forest.prepare_initial_context(self.actor()).await
    }

    pub(super) async fn acknowledge_initial_context_publication(
        &self,
        operation: &tidepool_runtime::session::WorkbenchExecutionId,
        publication: exomonad_actor::InitialContextPublication,
    ) -> Result<exomonad_actor::InitialContextObservation, ResidentActorWorkbenchError> {
        self.validate().await?;
        self.forest
            .acknowledge_initial_context_publication(self.actor(), operation, publication)
            .await
    }
}
