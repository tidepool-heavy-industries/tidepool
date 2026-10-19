//! Project observations from the exact executing installation and lifecycle owners.

use super::*;
use crate::generated::runtime_info::RuntimeInfoError;
use crate::resident_workbench::ActorRequestSnapshot;
use tidepool_bridge_effects::{
    RuntimeInference, RuntimeInfoSnapshot, RuntimeSpec, RuntimeStartup, RuntimeTool,
    RuntimeToolSchedule,
};

fn unavailable(detail: impl std::fmt::Display) -> RuntimeInfoError {
    RuntimeInfoError::RuntimeInfoUnavailable(detail.to_string())
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn runtime_info(
        &self,
        issued: Option<&ActorRequestSnapshot>,
    ) -> Result<RuntimeInfoSnapshot, RuntimeInfoError> {
        let issued = issued.ok_or_else(|| unavailable("execution has no issued installation"))?;
        let installation = issued.installation();
        let resident = self
            .lifecycle
            .as_ref()
            .and_then(|owner| owner.resident_snapshot())
            .ok_or_else(|| unavailable("execution has no resident lifecycle observation"))?;
        if resident.context.actor != installation.actor() {
            return Err(unavailable(
                "executing installation and lifecycle belong to different actors",
            ));
        }
        let executing_spec = installation
            .tools()
            .map(|tools| {
                Ok::<_, RuntimeInfoError>(RuntimeSpec {
                    install: i64::try_from(tools.install).map_err(unavailable)?,
                    entry: tools.resolved_spec().and_then(|spec| spec.entry.clone()),
                    revision: tools.revision().map(str::to_owned),
                    source: installation
                        .source()
                        .semantic_digest()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    tools: tools
                        .declaration_snapshot()
                        .iter()
                        .map(|tool| RuntimeTool {
                            name: tool.name().to_owned(),
                            schedule: match tool.scheduling() {
                                exomonad_tool::ToolScheduling::Async => {
                                    RuntimeToolSchedule::RuntimeAsynchronous
                                }
                                exomonad_tool::ToolScheduling::BeforeNextInference => {
                                    RuntimeToolSchedule::RuntimeBeforeNextInference
                                }
                            },
                            effects: tool
                                .effect_keys()
                                .iter()
                                .map(|key| key.wire_name().to_owned())
                                .collect(),
                        })
                        .collect(),
                })
            })
            .transpose()?;
        let executing_generation = issued
            .generation()
            .map(|generation| i64::try_from(generation.id().get()).map_err(unavailable))
            .transpose()?;
        let configured_inference =
            match (resident.descriptor.model(), resident.descriptor.reasoning()) {
                (Some(model), Some(reasoning)) => {
                    RuntimeInference::RuntimeConfiguredInference(model.clone(), reasoning)
                }
                _ => RuntimeInference::RuntimeInferenceUnavailable,
            };
        let startup = match resident.initial_context {
            Some(observation) => RuntimeStartup::RuntimeStartupObserved(
                super::initial_context::wire_initial_context(observation).map_err(unavailable)?,
            ),
            None => RuntimeStartup::RuntimeStartupNotAdmitted,
        };
        Ok(RuntimeInfoSnapshot {
            executing_spec,
            executing_generation,
            configured_inference,
            startup,
        })
    }
}
