//! A workbench task carries only execution-owned work across the actor turn.

use futures_util::future::BoxFuture;
use tidepool_runtime::session::WorkbenchResponse;

use super::{KernelInvocationFailure, KernelStep};

/// Work performed outside the actor turn. Its completion returns a typed
/// finalizer, so actor-owned state is changed only by the matching mailbox
/// completion after the step key has been checked.
pub struct OwnedWorkbenchTask<B> {
    run: BoxFuture<'static, OwnedWorkbenchCompletion<B>>,
}

impl<B> OwnedWorkbenchTask<B> {
    pub fn new(run: BoxFuture<'static, OwnedWorkbenchCompletion<B>>) -> Self {
        Self { run }
    }

    pub(super) fn into_future(self) -> BoxFuture<'static, OwnedWorkbenchCompletion<B>> {
        self.run
    }
}

/// The task's result and any execution state that must rejoin its owning
/// behavior. The finalizer runs on the actor, never on the worker task.
pub struct OwnedWorkbenchCompletion<B> {
    finish: Box<
        dyn FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure> + Send,
    >,
}

impl<B> OwnedWorkbenchCompletion<B> {
    pub fn new(
        finish: impl FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>
            + Send
            + 'static,
    ) -> Self {
        Self {
            finish: Box::new(finish),
        }
    }

    pub(super) fn finish(
        self,
        behavior: &mut B,
    ) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure> {
        (self.finish)(behavior)
    }
}

/// A workbench operation runs with execution-owned state or hands its intact
/// invocation to a behavior that must run sequentially.
pub enum WorkbenchDispatch<B> {
    Owned(OwnedWorkbenchTask<B>),
    Sequential {
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
    },
}
