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
    pub(super) fn admit(
        forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
        actor: ActorRef,
    ) -> Result<Self, ResidentActorWorkbenchError> {
        let admission = forest.authorize_provider_attachment(actor)?;
        Ok(Self { forest, admission })
    }

    pub(super) fn validate(&self) -> Result<(), ResidentActorWorkbenchError> {
        self.forest.validate_provider_attachment(&self.admission)
    }
}
