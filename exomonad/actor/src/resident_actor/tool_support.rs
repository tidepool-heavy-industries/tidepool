//! Instance support at the actor interpreter boundary.

use super::{DispatchEffect, OutputSink, ResidentEnvironment, ResidentForest};
use exomonad_tool::{ActorEffectKey, ToolEffectKey};

/// Observe interpreter structure from the actual installed service handles.
/// This does not establish external credentials or provider availability.
#[must_use]
pub fn installed_intrinsic_effect_support(
    source_layers: Option<&crate::ActorSourceLayerResolver>,
    jev: &crate::JevBackendHandle,
    models: Option<&std::sync::Arc<dyn crate::CellModelFactory>>,
    conversation: Option<&crate::ConversationReader>,
) -> Vec<ToolEffectKey> {
    crate::resident_workbench::intrinsic_effect_families()
        .into_iter()
        .filter(|effect| match effect {
            ToolEffectKey::Actor(ActorEffectKey::Source) => source_layers.is_some(),
            ToolEffectKey::Actor(ActorEffectKey::Jev) => jev.is_installed(),
            ToolEffectKey::Actor(key)
                if key.model_permission().is_some_and(|permission| {
                    permission.operation == crate::ModelOperation::ModelCall
                }) =>
            {
                models.is_some()
            }
            ToolEffectKey::Actor(ActorEffectKey::Reflect) => conversation.is_some(),
            _ => true,
        })
        .collect()
}

impl<H, O> ResidentEnvironment<H, O> {
    pub(super) fn application_workbench(&self) -> crate::ResidentActorWorkbench<H, O> {
        self.runner
            .application_workbench()
            .with_intrinsic_effect_support(self.intrinsic_effect_support())
    }

    pub(super) fn boundary_runner(&self) -> crate::ResidentActorRunner<H, O> {
        self.application_workbench().boundary_runner()
    }

    /// Request mounting and subsequent dispatch share interpreter composition;
    /// the request contributes its scope and compiler-issued type evidence.
    pub(super) fn request_workbench(
        &self,
        response: crate::ResponseExpectation,
        request: crate::RequestId,
        evidence: std::sync::Arc<tidepool_runtime::session::SiteTypeEvidence>,
    ) -> crate::ResidentActorWorkbench<H, O> {
        self.runner
            .workbench(response, request, evidence)
            .with_intrinsic_effect_support(self.intrinsic_effect_support())
    }

    pub(super) fn intrinsic_effect_support(&self) -> Vec<ToolEffectKey> {
        installed_intrinsic_effect_support(
            self.source_layers.as_ref(),
            &self.jev,
            self.cell_model_factory.as_ref(),
            self.conversation_reader.as_ref(),
        )
    }
}

impl<H, O> ResidentForest<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Observe the actual interpreter instances before admitting an actor.
    /// This is the same support used by hosted installation and cell dispatch.
    #[must_use]
    pub fn installed_intrinsic_effect_support(&self) -> Vec<ToolEffectKey> {
        self.environment.intrinsic_effect_support()
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    struct UnusedModelFactory;
    impl crate::CellModelFactory for UnusedModelFactory {
        fn bind(
            &self,
            _: &tidepool_runtime::session::WorkbenchExecutionId,
            _: tidepool_repr::PrincipalId,
            _: &crate::ActorDescriptor,
            _: Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
        ) -> std::sync::Arc<dyn crate::CellModelBinding> {
            unreachable!("support observation never allocates an execution binding")
        }
    }

    #[test]
    fn model_interpreter_support_covers_each_installed_family() {
        let factory: std::sync::Arc<dyn crate::CellModelFactory> =
            std::sync::Arc::new(UnusedModelFactory);
        for models in [None, Some(&factory)] {
            let supported =
                installed_intrinsic_effect_support(None, &crate::unconfigured_jev(), models, None);
            let model_keys = [
                ActorEffectKey::ModelCallOpenAI,
                ActorEffectKey::ModelCallArgon,
                ActorEffectKey::ModelCallDeepSeek,
            ];
            for key in model_keys {
                assert_eq!(
                    supported.contains(&ToolEffectKey::Actor(key)),
                    models.is_some()
                );
            }
        }
    }
}
