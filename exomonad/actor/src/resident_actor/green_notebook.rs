//! Notebook presentation around the common native Green invocation cursor.
use super::green::{Completion, GreenAdvance};
use super::*;

pub(super) struct EffectReceipt {
    display: Option<WorkbenchDisplayOutput>,
    success_disposition: WorkbenchOperationDisposition,
    ordinal: usize,
    effect: String,
    started: std::time::Instant,
    observed_child: Option<ActorRef>,
}

pub(super) struct EffectCompletion {
    receipt: EffectReceipt,
    disposition: WorkbenchOperationDisposition,
    started_job: Option<String>,
    retained_job_binding: Option<crate::resident_workbench::RetainedHostBinding>,
}

impl EffectReceipt {
    pub(super) fn split(pending: ParkedWorkbenchEffect) -> (OwnedWorkbenchWait, Self) {
        let observed_child = pending.wait.observe_after_resume();
        (
            pending.wait,
            Self {
                display: pending.display,
                success_disposition: pending.success_disposition,
                ordinal: pending.ordinal,
                effect: pending.effect,
                started: pending.started,
                observed_child,
            },
        )
    }
}

impl EffectReceipt {
    pub(super) fn attach(
        self,
        result: commands::CommandResolution,
    ) -> (
        Result<ResidentOutcome, ResidentActorWorkbenchError>,
        EffectCompletion,
    ) {
        (
            result.outcome,
            EffectCompletion {
                receipt: self,
                disposition: result.disposition,
                started_job: result.started_job,
                retained_job_binding: result.retained_job_binding,
            },
        )
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn prepare_green_boundary(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        owner: &CurrentEffectOwner<'_>,
        current: &mut WorkbenchFragmentExecution,
        boundary: green::GreenBoundary,
    ) -> Result<FragmentAdvance, ResidentActorWorkbenchError> {
        let mut green = current.green.take().unwrap_or_default();
        let advance = self.prepare_green_operation(
            kernel,
            context,
            owner,
            &mut green,
            &mut current.scopes,
            boundary,
        )?;
        current.green = Some(green);
        match advance {
            GreenAdvance::Start {
                callback,
                realm,
                token,
                work,
            } => {
                current.native_start =
                    Some(owned_workbench::WorkbenchFragmentRequest::ScopeStart {
                        fragment: current
                            .fragment
                            .take()
                            .expect("async child borrows original fragment"),
                        callback,
                        realm,
                        token,
                        work,
                    });
                Ok(FragmentAdvance::ParkNative)
            }
            GreenAdvance::Wait => Ok(FragmentAdvance::ParkGreen),
        }
    }

    pub(super) fn apply_green_completion(
        &mut self,
        state: &mut WorkbenchExecutionState,
        completion: Completion<EffectCompletion>,
    ) -> Result<bool, ResidentActorWorkbenchError> {
        let current = state
            .cursor
            .running
            .as_mut()
            .expect("original async fragment");
        let green = current
            .green
            .as_mut()
            .expect("invocation-local async frontiers");
        let Some(mut completion) =
            self.apply_green_frontier(&state.effects.context, green, completion)?
        else {
            return Ok(false);
        };
        let mut outcome = completion.result.take().expect("ready native frontier");
        current.scopes = completion.scopes;
        current.inflight_effect = None;
        if let Some(mut attachment) = completion.attachment {
            let receipt = attachment.receipt;
            if let Some(binding) = attachment.retained_job_binding.take() {
                if !state
                    .effects
                    .control
                    .as_ref()
                    .is_some_and(|control| control.cancellation_requested())
                {
                    match binding.accept() {
                        Ok(binding) => {
                            state.cursor.unit.recovered_bindings.push(binding.clone());
                            current
                                .fragment
                                .as_mut()
                                .expect("original command fragment")
                                .retain_job_binding(binding);
                        }
                        Err(error) => {
                            attachment.disposition = WorkbenchOperationDisposition::Unknown;
                            outcome = Err(error);
                        }
                    }
                }
            }
            record_workbench_operation(
                &mut state.cursor.unit.operations,
                state.request.execution_id(),
                state.cursor.index,
                receipt.ordinal,
                &receipt.effect,
                receipt.display,
                receipt.started.elapsed(),
                scoped_operation_disposition(receipt.success_disposition, attachment.disposition),
            );
            if let Some(job) = attachment.started_job {
                current
                    .fragment
                    .as_mut()
                    .expect("original command fragment")
                    .record_started_job(job);
            }
            if outcome.is_ok() {
                if let Some(child) = receipt.observed_child {
                    self.record_child_observation(child);
                }
            }
        }
        match outcome {
            Ok(outcome) => current.outcome = Some(outcome),
            Err(error) => current.resume_failure = Some(error),
        }
        Ok(true)
    }
}
