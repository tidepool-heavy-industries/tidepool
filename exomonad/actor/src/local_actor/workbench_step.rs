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

/// One execution's cleanup claim. Create it as soon as a private scope or
/// other resource is acquired, then move it into the completion. A worker
/// panic, cancelled future, stale result or stopped actor drops the claim.
pub struct WorkbenchAbandonGuard {
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl WorkbenchAbandonGuard {
    pub fn new(cleanup: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }

    fn disarm(&mut self) {
        self.cleanup.take();
    }
}

impl Drop for WorkbenchAbandonGuard {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup)).is_err() {
                tracing::error!("execution-owned workbench abandonment callback panicked");
            }
        }
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
    abandon_guard: Option<WorkbenchAbandonGuard>,
}

impl<B> OwnedWorkbenchCompletion<B> {
    pub fn new(
        finish: impl FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>
            + Send
            + 'static,
    ) -> Self {
        Self {
            finish: Some(Box::new(finish)),
            abandon_guard: None,
        }
    }

    /// Transfer the task's exact cleanup claim to its actor completion.
    pub fn with_abandon_guard(mut self, guard: WorkbenchAbandonGuard) -> Self {
        assert!(self.abandon_guard.is_none(), "one workbench cleanup owner");
        self.abandon_guard = Some(guard);
        self
    }

    pub(super) fn finish(
        mut self,
        behavior: &mut B,
    ) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure> {
        let result = self.finish.take().expect("one completion finalizer")(behavior);
        if result.is_ok() {
            if let Some(guard) = self.abandon_guard.as_mut() {
                guard.disarm();
            }
        }
        result
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
