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
    use std::sync::Arc;

    struct ObservationOnlySource;

    impl crate::SourceInspection for ObservationOnlySource {}

    impl crate::ActorSourceLayers for ObservationOnlySource {
        fn inspection_services(self: Arc<Self>) -> crate::SourceInspectionServices {
            crate::SourceInspectionServices::new(self)
        }

        fn stage_reload(
            self: Arc<Self>,
            _: tidepool_repr::PrincipalId,
            _: &crate::CheckpointSourceLayer,
            _: &[String],
        ) -> Result<Box<dyn crate::StagedActorSourceReload>, crate::SourceLayerReload> {
            unreachable!("support observation never prepares source")
        }
    }

    struct ObservationOnlyJev;

    impl crate::JevBackend for ObservationOnlyJev {
        fn ask(
            &self,
            _: String,
        ) -> futures_util::future::BoxFuture<'_, Result<String, crate::JevCallFailure>> {
            unreachable!("support observation never invokes a provider")
        }
    }

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

    #[tokio::test]
    async fn composed_workbench_support_survives_boundary_runner_extraction() {
        // Exercise each service independently: another installed service must
        // not make an absent interpreter appear available.
        for installed in 0u8..16 {
            let machine = tidepool_runtime::session::ResidentSession::unbootstrapped(
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                None,
            );
            let (mut forest, _deployments) = ResidentForest::new(
                crate::ActorWorkbenchSource::new("", Vec::new()),
                tidepool_runtime::session::fresh_session_id(),
                machine,
                None,
                crate::Incarnation::FIRST,
            )
            .await
            .unwrap();
            if installed & 1 != 0 {
                forest.set_source_layers(Arc::new(ObservationOnlySource));
            }
            if installed & 2 != 0 {
                forest.set_jev_backend(Arc::new(ObservationOnlyJev));
            }
            if installed & 4 != 0 {
                forest = forest.with_cell_model_factory(Arc::new(UnusedModelFactory));
            }
            if installed & 8 != 0 {
                forest = forest.with_conversation_reader(Arc::new(|_, _| {
                    unreachable!("support observation never reads a conversation")
                }));
            }

            let workbench = forest.environment.application_workbench();
            let runner = workbench.boundary_runner();
            let restored = runner.application_workbench().boundary_runner();
            for (boundary, runner) in [("extracted", runner), ("restored", restored)] {
                let source = runner.preparation_source();
                let support = source.config().installed_effect_support();
                for (key, flag) in [
                    (ActorEffectKey::Source, 1),
                    (ActorEffectKey::Jev, 2),
                    (ActorEffectKey::ModelCallOpenAI, 4),
                    (ActorEffectKey::ModelCallArgon, 4),
                    (ActorEffectKey::ModelCallDeepSeek, 4),
                    (ActorEffectKey::Reflect, 8),
                ] {
                    assert_eq!(
                        support.contains(&ToolEffectKey::Actor(key)),
                        installed & flag != 0,
                        "{boundary} support for {key:?}, installed service mask {installed}"
                    );
                }
            }
            let shutdown = forest.shutdown().await;
            assert!(
                shutdown.iter().all(crate::ForestRootShutdown::is_confirmed),
                "service mask {installed} cleanup: {shutdown:?}"
            );
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
