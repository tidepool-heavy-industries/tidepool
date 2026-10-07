//! Invocation-local authority for synchronous context transformations.

use exomonad_tool::ToolInvocationContext;
use tidepool_effect::{error::EffectError, DeferredEffect};
use tidepool_repr::{DataConTable, PrincipalId};
use tidepool_runtime::session::WorkbenchExecutionId;

pub use crate::generated::context_read_write::ContextReadWriteReq as ContextReq;

/// How the entire authored invocation terminated, independently of its display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellExitCause {
    FullReturn,
    ReplyTransfer,
    Backgrounded,
    Rejected,
    Cancelled,
    Failed,
}

/// Produced after native, model, and invocation owners have settled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellExit {
    pub execution: WorkbenchExecutionId,
    pub cause: CellExitCause,
    pub cleanup_confirmed: bool,
}

impl CellExit {
    pub fn permits_context_commit(&self) -> bool {
        self.cause == CellExitCause::FullReturn && self.cleanup_confirmed
    }

    pub(crate) fn from_reply(
        execution: WorkbenchExecutionId,
        result: &Result<
            crate::KernelStep<tidepool_runtime::session::WorkbenchResponse>,
            crate::KernelInvocationFailure,
        >,
        cleanup_confirmed: bool,
        cancelled: bool,
    ) -> Self {
        use tidepool_runtime::session::WorkbenchRunStatus;
        let cause = if cancelled {
            CellExitCause::Cancelled
        } else {
            match result {
                Err(crate::KernelInvocationFailure::Cancelled { .. }) => CellExitCause::Cancelled,
                Err(crate::KernelInvocationFailure::Rejected { .. }) => CellExitCause::Rejected,
                Err(_) => CellExitCause::Failed,
                Ok(step) => {
                    let response = match step {
                        crate::KernelStep::Continue(response)
                        | crate::KernelStep::ContinueLater(response)
                        | crate::KernelStep::Stop {
                            output: response, ..
                        } => response,
                    };
                    match response.status {
                        WorkbenchRunStatus::Committed | WorkbenchRunStatus::Completed
                            if response.next_index == response.total =>
                        {
                            CellExitCause::FullReturn
                        }
                        WorkbenchRunStatus::Replied => CellExitCause::ReplyTransfer,
                        WorkbenchRunStatus::RequestCancelled => CellExitCause::Cancelled,
                        WorkbenchRunStatus::Rejected => CellExitCause::Rejected,
                        WorkbenchRunStatus::Backgrounded => CellExitCause::Backgrounded,
                        _ => CellExitCause::Failed,
                    }
                }
            }
        };
        Self {
            execution,
            cause,
            cleanup_confirmed,
        }
    }
}

/// The host passes one exact capability beside the admitted invocation. Saved
/// context values and actor effect membership never supply this authority.
/// Finalization records eligibility; publication remains the Engine/Store owner.
pub trait HostedContextBinding: Send + Sync {
    fn admit(
        &self,
        execution: &WorkbenchExecutionId,
        invocation: &ToolInvocationContext,
        principal: PrincipalId,
    ) -> Result<(), EffectError>;

    fn prepare(
        &self,
        request: ContextReq,
        principal: PrincipalId,
        table: DataConTable,
    ) -> DeferredEffect;

    /// Revoke draft authority without awaiting Store or model work.
    fn cancel(&self);

    /// Close mutation admission and retain this exact terminal receipt.
    fn finish(&self, exit: CellExit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};

    fn exit(
        status: WorkbenchRunStatus,
        next_index: usize,
        cleanup: bool,
        cancelled: bool,
    ) -> CellExit {
        let reply = Ok(crate::KernelStep::Continue(WorkbenchResponse {
            publication: None,
            status,
            summary: None,
            items: Vec::new(),
            next_index,
            total: 2,
        }));
        CellExit::from_reply(
            WorkbenchExecutionId::from_digest([1; 16]),
            &reply,
            cleanup,
            cancelled,
        )
    }

    #[test]
    fn only_whole_return_with_confirmed_cleanup_allows_context_commit() {
        for status in [WorkbenchRunStatus::Committed, WorkbenchRunStatus::Completed] {
            assert!(exit(status, 2, true, false).permits_context_commit());
            assert!(!exit(status, 1, true, false).permits_context_commit());
            assert!(!exit(status, 2, false, false).permits_context_commit());
        }
        for status in [
            WorkbenchRunStatus::Backgrounded,
            WorkbenchRunStatus::Replied,
            WorkbenchRunStatus::Rejected,
            WorkbenchRunStatus::RequestCancelled,
        ] {
            assert!(!exit(status, 2, true, false).permits_context_commit());
        }
    }

    #[test]
    fn context_cancellation_wins_even_after_machine_publication() {
        let receipt = exit(WorkbenchRunStatus::Committed, 2, true, true);
        assert_eq!(receipt.cause, CellExitCause::Cancelled);
        assert!(!receipt.permits_context_commit());
    }

    #[test]
    fn typed_invocation_cancellation_is_distinct_from_failure_and_requires_cleanup() {
        let actor = crate::ActorRef::first(crate::ActorId(9));
        let execution = WorkbenchExecutionId::from_digest([2; 16]);
        for cleanup_confirmed in [false, true] {
            let reply = Err(crate::KernelInvocationFailure::Cancelled { actor });
            let exit = CellExit::from_reply(execution.clone(), &reply, cleanup_confirmed, false);
            assert_eq!(exit.cause, CellExitCause::Cancelled);
            assert_eq!(exit.cleanup_confirmed, cleanup_confirmed);
            assert!(!exit.permits_context_commit());
            assert!(reply.unwrap_err().receipts().is_empty());
        }
        let failure = Err(crate::KernelInvocationFailure::Failed {
            actor,
            diagnostic: None,
            detail: "invalid child terminal".into(),
            receipts: Vec::new(),
        });
        assert_eq!(
            CellExit::from_reply(execution, &failure, true, false).cause,
            CellExitCause::Failed
        );
    }
}
