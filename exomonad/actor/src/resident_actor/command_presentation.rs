use super::*;
use tidepool_bridge_effects::{CommandError, CommandPage, CommandStream};

pub(super) struct CommandPresentationRequest {
    pub job: String,
    pub presentation: CommandPresentation,
    pub summarize: bool,
    pub named_tool: bool,
    pub display_remaining: usize,
}

pub(super) struct PreparedCommandPresentation {
    job: String,
    presentation: CommandPresentation,
    binding: Option<String>,
    displayed_pages: Option<Vec<(CommandStream, CommandPage)>>,
}

impl PreparedCommandPresentation {
    /// Runs only after the original owned-step fence, on its same fragment.
    pub(super) fn apply(
        self,
        jobs: &crate::command_jobs::CommandJobs,
        actor: ActorRef,
        fragment: &mut ResidentWorkbenchFragment,
        remaining: &mut usize,
        output: &mut Vec<String>,
        recovered_bindings: &mut Vec<String>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if let Some(binding) = self.binding {
            recovered_bindings.push(binding.clone());
            fragment.retain_job_binding(binding);
        }
        let rendered = fragment.present_command(self.job.clone(), self.presentation, remaining);
        if !rendered.is_empty() {
            output.push(rendered);
        }
        if let Some(pages) = self.displayed_pages {
            jobs.mark_displayed(actor, &self.job, &pages)
                .map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "command observation receipt: {error:?}"
                    ))
                })?;
        }
        Ok(())
    }
}

pub(super) async fn prepare<H, O>(
    jobs: &crate::command_jobs::CommandJobs,
    context: &ActorSessionContext,
    workbench: &crate::ResidentActorWorkbench<H, O>,
    request: CommandPresentationRequest,
    permitted: bool,
) -> Result<PreparedCommandPresentation, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    if !permitted {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "command presentation is not authorized".into(),
        ));
    }
    let CommandPresentationRequest {
        job,
        mut presentation,
        summarize,
        named_tool,
        display_remaining,
    } = request;
    if summarize && matches!(presentation, CommandPresentation::CommandVisible(_, _)) {
        let status = jobs.status(context.actor, &job).await;
        let output = jobs.output(context.actor, &job, 0).await;
        let exit = match status {
            Ok(tidepool_bridge_effects::CommandStatus::CommandFinished(result)) => {
                match result.outcome {
                    tidepool_bridge_effects::CommandOutcome::CommandExited(code) => {
                        format!("exit {code}")
                    }
                    other => format!("{other:?}"),
                }
            }
            Ok(tidepool_bridge_effects::CommandStatus::CommandQueued) => "queued".into(),
            Ok(tidepool_bridge_effects::CommandStatus::CommandStarting) => "starting".into(),
            Ok(tidepool_bridge_effects::CommandStatus::CommandRunning) => "running".into(),
            Ok(tidepool_bridge_effects::CommandStatus::CommandStopping) => "stopping".into(),
            Err(error) => format!("status unavailable: {error:?}"),
        };
        let counts = match output {
            Ok(output) => format!(
                "stdout {} bytes · stderr {} bytes",
                output.stdout.available_end, output.stderr.available_end
            ),
            Err(error) => format!("stdout/stderr byte counts unavailable: {error:?}"),
        };
        presentation =
            CommandPresentation::CommandVisible(format!("command {job}: {exit} · {counts}"), 512);
    }
    let limit = match &presentation {
        CommandPresentation::CommandVisible(_, bytes) => usize::try_from(*bytes)
            .unwrap_or(0)
            .min(65536)
            .min(display_remaining),
        CommandPresentation::CommandQuiet => 0,
    };
    let pages =
        if matches!(presentation, CommandPresentation::CommandVisible(_, _)) && limit >= 1024 {
            Some(jobs.observation(context.actor, &job).await)
        } else {
            None
        };
    if let CommandPresentation::CommandVisible(text, _) = &mut presentation {
        *text = crate::workbench_display::bounded_output(text, 512);
        match &pages {
            Some(Ok(pages)) => text.push_str(&crate::workbench_display::command_pages(pages)),
            Some(Err(CommandError::CommandOutputPending)) => {
                text.push_str("\nNo output yet; streams are starting.")
            }
            Some(Err(error)) => text.push_str(&format!(
                "\nOutput observation unavailable: {error:?}. Inspect the same job; do not rerun for output."
            )),
            None => {}
        }
    }
    let mut shortened = false;
    let mut binding = None;
    if named_tool {
        if let CommandPresentation::CommandVisible(text, _) = &mut presentation {
            let incomplete = pages.as_ref().is_some_and(|pages| match pages {
                Ok(pages) => pages
                    .iter()
                    .any(|(_, page)| page.lost_bytes > 0 || page.end < page.available_end),
                Err(CommandError::CommandOutputPending) => false,
                Err(_) => true,
            });
            let oversized = text.len() > limit || incomplete;
            shortened = oversized;
            let retained = workbench
                .bind_command_job(context.clone(), job.clone())
                .await?;
            if oversized {
                shortened |= text.len() > (8 * 1024).min(limit).saturating_sub(512);
                *text = crate::workbench_display::bounded_output(
                    text,
                    (8 * 1024).min(limit).saturating_sub(512),
                );
                *text = format!(
                    "retained as {retained} :: Cmd.Job\nnext: read_output session_id={job}, stream=Stdout (or Stderr), offset=0. Do not rerun.\n{text}"
                );
            } else {
                *text = format!("retained as {retained} :: Cmd.Job\n{text}");
            }
            binding = Some(retained);
        }
    }
    if let CommandPresentation::CommandVisible(text, _) = &mut presentation {
        shortened |= text.len() > limit;
        *text = crate::workbench_display::bounded_output(text, limit);
    }
    Ok(PreparedCommandPresentation {
        job,
        presentation,
        binding,
        displayed_pages: pages.filter(|_| !shortened).and_then(Result::ok),
    })
}

pub(super) async fn retain_job_binding<H, O>(
    jobs: &crate::command_jobs::CommandJobs,
    context: &ActorSessionContext,
    workbench: &crate::ResidentActorWorkbench<H, O>,
    job: String,
    permitted: bool,
) -> (Result<String, CommandError>, Option<String>)
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let result = if !permitted {
        Err(CommandError::CommandUnauthorized)
    } else {
        match jobs.owner(&job) {
            Err(error) => Err(error),
            Ok(owner) => authorize_job_binding(context.actor, owner),
        }
    };
    let result = match result {
        Err(error) => Err(error),
        Ok(()) => workbench
            .bind_command_job(context.clone(), job)
            .await
            .map_err(|error| CommandError::CommandUnavailable(error.to_string())),
    };
    let binding = result.as_ref().ok().cloned();
    (result, binding)
}

fn authorize_job_binding(caller: ActorRef, owner: ActorRef) -> Result<(), CommandError> {
    if caller == owner {
        Ok(())
    } else {
        Err(CommandError::CommandUnauthorized)
    }
}

#[cfg(test)]
mod binding_authority_tests {
    use super::*;

    #[test]
    fn foreign_job_owner_cannot_retain_the_binding() {
        let caller = ActorRef::first(crate::ActorId(1));
        let owner = ActorRef::first(crate::ActorId(2));
        assert_eq!(
            authorize_job_binding(caller, owner),
            Err(CommandError::CommandUnauthorized)
        );
        assert_eq!(
            authorize_job_binding(owner, owner),
            Ok(()),
            "the owning actor may retain its command job"
        );
    }
}
