//! Shared native effect admission for notebook and exclusive tool frontiers.
use super::*;

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn prepare_invocation_wait(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        boundary: ResidentActorBoundary,
    ) -> Result<Result<OwnedWorkbenchWait, ResidentActorBoundary>, ResidentActorWorkbenchError>
    {
        Ok(match boundary {
            ResidentActorBoundary::Drain {
                continuation,
                target,
            } => Ok(OwnedWorkbenchWait::Drain {
                continuation,
                target: self.capture_drain_target(kernel, context, target)?,
            }),
            ResidentActorBoundary::Wait(wait) => {
                let terminal = self.capture_exit_target(kernel, effect_owner, wait.target)?;
                Ok(OwnedWorkbenchWait::Exit {
                    continuation: wait.continuation,
                    target: wait.target,
                    terminal,
                })
            }
            ResidentActorBoundary::Poll(poll) => {
                let terminal = kernel
                    .resolve(poll.target)
                    .map(|target| target.terminal().clone())
                    .filter(|terminal| terminal.get().is_some());
                Ok(OwnedWorkbenchWait::PollExit {
                    continuation: poll.continuation,
                    target: poll.target,
                    terminal,
                })
            }
            boundary => {
                match self.prepare_independent_effect(kernel, context, effect_owner, boundary) {
                    Ok(operation) => Ok(OwnedWorkbenchWait::Prepared(operation)),
                    Err(boundary) => OwnedWorkbenchWait::capture(boundary),
                }
            }
        })
    }
}
