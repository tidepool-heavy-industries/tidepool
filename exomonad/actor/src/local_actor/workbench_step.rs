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
    finish: Option<
        Box<
            dyn FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>
                + Send,
        >,
    >,
    on_abandoned: Option<Box<dyn FnOnce() + Send>>,
}

impl<B> OwnedWorkbenchCompletion<B> {
    pub fn new(
        finish: impl FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>
            + Send
            + 'static,
    ) -> Self {
        Self {
            finish: Some(Box::new(finish)),
            on_abandoned: None,
        }
    }

    /// Queue exact execution cleanup if the actor stops, ignores a stale
    /// completion, or its finalizer fails before claiming the result.
    pub fn on_abandoned(mut self, cleanup: impl FnOnce() + Send + 'static) -> Self {
        assert!(self.on_abandoned.is_none(), "one workbench cleanup owner");
        self.on_abandoned = Some(Box::new(cleanup));
        self
    }

    pub(super) fn finish(
        mut self,
        behavior: &mut B,
    ) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure> {
        let result = self.finish.take().expect("one completion finalizer")(behavior);
        if result.is_ok() {
            self.on_abandoned.take();
        }
        result
    }
}

impl<B> Drop for OwnedWorkbenchCompletion<B> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.on_abandoned.take() {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup)).is_err() {
                tracing::error!("execution-owned workbench abandonment callback panicked");
            }
        }
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
