//! Owned actor tasks carry one admitted execution across fenced actor steps.

use futures_util::future::BoxFuture;
use tidepool_runtime::session::WorkbenchResponse;

use super::{KernelContext, KernelInvocationFailure, KernelStep};

/// Concrete hosted workbench output on the shared actor task implementation.
pub type OwnedWorkbenchTask<B> = OwnedActorTask<B, WorkbenchResponse>;
pub type OwnedWorkbenchCompletion<B> = OwnedActorCompletion<B, WorkbenchResponse>;
pub type WorkbenchAdvance<B> = ActorAdvance<B, WorkbenchResponse>;
pub type WorkbenchAbandonGuard = ActorAbandonGuard;

/// Work performed outside the actor turn. Its completion returns a typed
/// finalizer, so actor-owned state is changed only by the matching mailbox
/// completion after the step key has been checked.
pub struct OwnedActorTask<B, T> {
    run: ActorTaskExecution<B, T>,
    abandon_guard: Option<ActorAbandonGuard>,
}

pub(super) enum ActorTaskExecution<B, T> {
    Owned(BoxFuture<'static, OwnedActorCompletion<B, T>>),
    /// Temporary transfer for handlers still using the existing serial driver.
    /// It resumes the retained cursor; it cannot admit an invocation again.
    Serial(
        Box<
            dyn FnOnce(
                    B,
                    std::sync::Arc<KernelContext>,
                ) -> BoxFuture<'static, (B, OwnedActorCompletion<B, T>)>
                + Send,
        >,
    ),
}

impl<B: 'static, T: 'static> OwnedActorTask<B, T> {
    pub fn new(run: BoxFuture<'static, OwnedActorCompletion<B, T>>) -> Self {
        Self {
            run: ActorTaskExecution::Owned(run),
            abandon_guard: None,
        }
    }

    pub(crate) fn serial(
        run: impl FnOnce(
                B,
                std::sync::Arc<KernelContext>,
            ) -> BoxFuture<'static, (B, OwnedActorCompletion<B, T>)>
            + Send
            + 'static,
    ) -> Self {
        Self {
            run: ActorTaskExecution::Serial(Box::new(run)),
            abandon_guard: None,
        }
    }

    pub(super) fn into_execution(self) -> ActorTaskExecution<B, T>
    where
        B: Send,
    {
        let guard = self.abandon_guard;
        match self.run {
            ActorTaskExecution::Owned(run) => ActorTaskExecution::Owned(Box::pin(async move {
                let mut completion = run.await;
                completion.inherit_guard(guard);
                completion
            })),
            ActorTaskExecution::Serial(run) => {
                ActorTaskExecution::Serial(Box::new(move |behavior, context| {
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
pub struct ActorAbandonGuard {
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl ActorAbandonGuard {
    pub fn new(cleanup: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }

    fn disarm(&mut self) {
        self.cleanup.take();
    }
}

impl Drop for ActorAbandonGuard {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup)).is_err() {
                tracing::error!("execution-owned actor task abandonment callback panicked");
            }
        }
    }
}

/// A terminal result or the next task of the same admitted execution.
pub enum ActorAdvance<B, T> {
    /// Settle the retained caller and its original execution control.
    Complete(KernelStep<T>),
    /// Keep admission pending and transfer ownership to the next task.
    Park(OwnedActorTask<B, T>),
}

type ActorResume<B, T> = Box<
    dyn FnOnce(&mut B, &KernelContext) -> Result<ActorAdvance<B, T>, KernelInvocationFailure>
        + Send,
>;

enum CompletionFinalizer<B, T> {
    Immediate(Box<dyn FnOnce(&mut B) -> Result<KernelStep<T>, KernelInvocationFailure> + Send>),
    Resume(ActorResume<B, T>),
}

/// The task result rejoins its owning behavior only after its step is fenced.
pub struct OwnedActorCompletion<B, T> {
    finish: Option<CompletionFinalizer<B, T>>,
    abandon_guard: Option<ActorAbandonGuard>,
}

impl<B, T> OwnedActorCompletion<B, T> {
    fn inherit_guard(&mut self, guard: Option<ActorAbandonGuard>) {
        if let Some(guard) = guard {
            assert!(self.abandon_guard.is_none(), "one actor task cleanup owner");
            self.abandon_guard = Some(guard);
        }
    }
    pub fn new(
        finish: impl FnOnce(&mut B) -> Result<KernelStep<T>, KernelInvocationFailure> + Send + 'static,
    ) -> Self {
        Self {
            finish: Some(CompletionFinalizer::Immediate(Box::new(finish))),
            abandon_guard: None,
        }
    }

    /// Resume local actor decisions after an exactly fenced owned step.
    /// Compilation and selected external waits return another owned task.
    pub(crate) fn advance(
        finish: impl FnOnce(&mut B, &KernelContext) -> Result<ActorAdvance<B, T>, KernelInvocationFailure>
            + Send
            + 'static,
    ) -> Self {
        Self {
            finish: Some(CompletionFinalizer::Resume(Box::new(finish))),
            abandon_guard: None,
        }
    }

    /// Transfer the task's exact cleanup claim to its actor completion.
    pub fn with_abandon_guard(mut self, guard: ActorAbandonGuard) -> Self {
        assert!(self.abandon_guard.is_none(), "one actor task cleanup owner");
        self.abandon_guard = Some(guard);
        self
    }

    pub(super) fn finish(
        mut self,
        behavior: &mut B,
        context: &KernelContext,
    ) -> Result<ActorAdvance<B, T>, KernelInvocationFailure> {
        let mut result = match self.finish.take().expect("one completion finalizer") {
            CompletionFinalizer::Immediate(finish) => finish(behavior).map(ActorAdvance::Complete),
            CompletionFinalizer::Resume(finish) => finish(behavior, context),
        };
        match &mut result {
            Ok(ActorAdvance::Complete(_)) => {
                if let Some(guard) = self.abandon_guard.as_mut() {
                    guard.disarm();
                }
            }
            Ok(ActorAdvance::Park(task)) => {
                if let Some(guard) = self.abandon_guard.take() {
                    assert!(task.abandon_guard.is_none(), "one actor task cleanup owner");
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
