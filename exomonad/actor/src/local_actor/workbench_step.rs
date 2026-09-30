//! A workbench task carries only execution-owned work across the actor turn.

use futures_util::future::BoxFuture;
use tidepool_runtime::session::WorkbenchResponse;

use super::{KernelContext, KernelInvocationFailure, KernelStep};

/// Work performed outside the actor turn. Its completion returns a typed
/// finalizer, so actor-owned state is changed only by the matching mailbox
/// completion after the step key has been checked.
pub struct OwnedWorkbenchTask<B> {
    run: WorkbenchTaskExecution<B>,
    abandon_guard: Option<WorkbenchAbandonGuard>,
}

pub(super) enum WorkbenchTaskExecution<B> {
    Owned(BoxFuture<'static, OwnedWorkbenchCompletion<B>>),
    /// Temporary transfer for handlers still using the existing serial driver.
    /// It resumes the retained cursor; it cannot admit an invocation again.
    Serial(
        Box<
            dyn FnOnce(
                    B,
                    std::sync::Arc<KernelContext>,
                ) -> BoxFuture<'static, (B, OwnedWorkbenchCompletion<B>)>
                + Send,
        >,
    ),
}

impl<B: 'static> OwnedWorkbenchTask<B> {
    pub fn new(run: BoxFuture<'static, OwnedWorkbenchCompletion<B>>) -> Self {
        Self {
            run: WorkbenchTaskExecution::Owned(run),
            abandon_guard: None,
        }
    }

    pub(crate) fn serial(
        run: impl FnOnce(
                B,
                std::sync::Arc<KernelContext>,
            ) -> BoxFuture<'static, (B, OwnedWorkbenchCompletion<B>)>
            + Send
            + 'static,
    ) -> Self {
        Self {
            run: WorkbenchTaskExecution::Serial(Box::new(run)),
            abandon_guard: None,
        }
    }

    pub(super) fn into_execution(self) -> WorkbenchTaskExecution<B>
    where
        B: Send,
    {
        let guard = self.abandon_guard;
        match self.run {
            WorkbenchTaskExecution::Owned(run) => {
                WorkbenchTaskExecution::Owned(Box::pin(async move {
                    let mut completion = run.await;
                    completion.inherit_guard(guard);
                    completion
                }))
            }
            WorkbenchTaskExecution::Serial(run) => {
                WorkbenchTaskExecution::Serial(Box::new(move |behavior, context| {
                    Box::pin(async move {
                        let (behavior, mut completion) = run(behavior, context).await;
                        completion.inherit_guard(guard);
                        (behavior, completion)
                    })
                }))
            }
        }
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

/// A terminal hosted result or the next task of the same admitted execution.
pub enum WorkbenchAdvance<B> {
    /// Settle the retained hosted caller and its control.
    Complete(KernelStep<WorkbenchResponse>),
    /// Keep the hosted caller pending and transfer ownership to the next task.
    Park(OwnedWorkbenchTask<B>),
}

type ActorResume<B> = Box<
    dyn FnOnce(&mut B, &KernelContext) -> Result<WorkbenchAdvance<B>, KernelInvocationFailure>
        + Send,
>;

enum CompletionFinalizer<B> {
    Immediate(
        Box<
            dyn FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>
                + Send,
        >,
    ),
    Resume(ActorResume<B>),
}

/// The task result rejoins its owning behavior only after its step is fenced.
pub struct OwnedWorkbenchCompletion<B> {
    finish: Option<CompletionFinalizer<B>>,
    abandon_guard: Option<WorkbenchAbandonGuard>,
}

impl<B> OwnedWorkbenchCompletion<B> {
    fn inherit_guard(&mut self, guard: Option<WorkbenchAbandonGuard>) {
        if let Some(guard) = guard {
            assert!(self.abandon_guard.is_none(), "one workbench cleanup owner");
            self.abandon_guard = Some(guard);
        }
    }
    pub fn new(
        finish: impl FnOnce(&mut B) -> Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>
            + Send
            + 'static,
    ) -> Self {
        Self {
            finish: Some(CompletionFinalizer::Immediate(Box::new(finish))),
            abandon_guard: None,
        }
    }

    /// Resume local actor decisions after an exactly fenced owned step.
    /// Compilation and selected external waits return another owned task.
    pub(crate) fn advance(
        finish: impl FnOnce(&mut B, &KernelContext) -> Result<WorkbenchAdvance<B>, KernelInvocationFailure>
            + Send
            + 'static,
    ) -> Self {
        Self {
            finish: Some(CompletionFinalizer::Resume(Box::new(finish))),
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
        context: &KernelContext,
    ) -> Result<WorkbenchAdvance<B>, KernelInvocationFailure> {
        let mut result = match self.finish.take().expect("one completion finalizer") {
            CompletionFinalizer::Immediate(finish) => {
                finish(behavior).map(WorkbenchAdvance::Complete)
            }
            CompletionFinalizer::Resume(finish) => finish(behavior, context),
        };
        match &mut result {
            Ok(WorkbenchAdvance::Complete(_)) => {
                if let Some(guard) = self.abandon_guard.as_mut() {
                    guard.disarm();
                }
            }
            Ok(WorkbenchAdvance::Park(task)) => {
                if let Some(guard) = self.abandon_guard.take() {
                    assert!(task.abandon_guard.is_none(), "one workbench cleanup owner");
                    task.abandon_guard = Some(guard);
                }
            }
            Err(_) => {}
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
