//! Instance support at the actor interpreter boundary.

use super::{DispatchEffect, OutputSink, ResidentEnvironment, ResidentForest};
use exomonad_tool::{ActorEffectKey, ToolEffectKey};

impl<H, O> ResidentEnvironment<H, O> {
    pub(super) fn intrinsic_effect_support(&self) -> Vec<ToolEffectKey> {
        crate::resident_workbench::intrinsic_effect_families()
            .into_iter()
            .filter(|effect| match effect {
                ToolEffectKey::Actor(ActorEffectKey::Jev) => self.jev.is_installed(),
                ToolEffectKey::Actor(ActorEffectKey::ModelCall) => {
                    self.cell_model_factory.is_some()
                }
                ToolEffectKey::Actor(ActorEffectKey::Reflect) => self.conversation_reader.is_some(),
                _ => true,
            })
            .collect()
    }
}

impl<H, O> ResidentForest<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Observe the interpreters installed on each exact machine checkout.
    /// Dedicated children may have different support from their parent.
    #[must_use]
    pub fn with_handler_effect_support(
        mut self,
        observer: impl Fn(&H) -> Vec<ToolEffectKey> + Send + Sync + 'static,
    ) -> Self {
        self.environment.runner = self
            .environment
            .runner
            .with_handler_effect_support(observer);
        self
    }
}
