use std::sync::Arc;

use exomonad_actor::{ActorCapabilities, ActorDescriptor, ActorRuntimeObservation};
use exomonad_tool::HostedTool;
use tidepool_runtime::session::ComposedPromptSnapshot;

use crate::exomonad::source::{CapturedPromptFiles, ExomonadSourceReload};

use super::prompt_catalog::{FrozenBasePrompt, JevSurface, PromptId};
use super::{append_capabilities, append_inheritance_authority, orient_launch_instructions};

fn compose(
    files: &CapturedPromptFiles,
    capabilities: &ActorCapabilities,
    explicit_instructions: Option<&str>,
    tools: &[HostedTool],
    observation: &ActorRuntimeObservation,
    jev: JevSurface,
) -> ComposedPromptSnapshot {
    let mut developer = append_capabilities(
        explicit_instructions
            .or(files.agent())
            .unwrap_or_else(|| PromptId::Agent.body())
            .to_owned(),
        capabilities,
    );
    append_inheritance_authority(&mut developer);
    let base = FrozenBasePrompt::selected_body(files.base(), files.api_guide(), jev);
    ComposedPromptSnapshot::capture(orient_launch_instructions(
        &format!("{base}\n\n{developer}"),
        observation,
        tools,
    ))
}

pub(super) fn install(
    descriptor: ActorDescriptor,
    config: &super::ActorHostConfig,
    source: Option<Arc<ExomonadSourceReload>>,
) -> ActorDescriptor {
    let files = CapturedPromptFiles::from_frozen(config.workspace_inputs.as_ref());
    let jev = config.jev_surface();
    let descriptor = descriptor.with_prompt_reload_preparer(
        move |actor, capabilities, explicit, tools, observation| {
            let observation = observation.snapshot();
            if let Some(source) = &source {
                let capabilities = capabilities.clone();
                let explicit = explicit.map(str::to_owned);
                let tools = tools.to_vec();
                let observation = observation.clone();
                source.set_prompt_composer(
                    actor.into(),
                    Arc::new(move |files| {
                        Ok(compose(
                            files,
                            &capabilities,
                            explicit.as_deref(),
                            &tools,
                            &observation,
                            jev,
                        ))
                    }),
                )?;
            }
            Ok(())
        },
    );
    descriptor.with_prompt_composer(move |_, capabilities, explicit, tools, observation| {
        Ok(compose(
            &files,
            capabilities,
            explicit,
            tools,
            &observation.snapshot(),
            jev,
        ))
    })
}
