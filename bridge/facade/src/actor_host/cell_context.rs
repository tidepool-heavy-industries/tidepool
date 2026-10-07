//! Per-invocation draft authority; durable publication belongs to the harness Store.
use std::sync::Arc;

use exomonad_actor::{CellExit, ContextReq, HostedContextBinding};
use exomonad_tool::ToolInvocationContext;
use harness::{
    context::{ContextDraft, ContextSnapshot},
    model::Effort,
    provider::{ContextDisposition, InvocationCompletionSource, ProviderCompletion},
    turn::JobOutput,
};
use parking_lot::Mutex;
use tidepool_effect::{error::EffectError, DeferredEffect, Response};
use tidepool_repr::{DataConTable, PrincipalId};
use tidepool_runtime::session::WorkbenchExecutionId;

pub(super) type ModelResolver = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

pub(super) struct EmbeddedContextBinding {
    invocation: ToolInvocationContext,
    model: ModelResolver,
    state: Arc<Mutex<DraftState>>,
}

struct DraftState {
    lifecycle: DraftLifecycle,
    draft: ContextDraft,
    changed: bool,
}

enum DraftLifecycle {
    Waiting,
    Active {
        execution: WorkbenchExecutionId,
        principal: PrincipalId,
    },
    Finished {
        exit: CellExit,
        draft: DraftEligibility,
    },
    Cancelled {
        execution: Option<WorkbenchExecutionId>,
    },
}

#[derive(Clone, Copy)]
enum DraftEligibility {
    Retained,
    Revoked,
}

impl EmbeddedContextBinding {
    pub(super) fn new(
        invocation: ToolInvocationContext,
        snapshot: &ContextSnapshot,
        model: ModelResolver,
    ) -> Self {
        Self {
            invocation,
            model,
            state: Arc::new(Mutex::new(DraftState {
                lifecycle: DraftLifecycle::Waiting,
                draft: ContextDraft {
                    document: snapshot.document.clone(),
                    next_model: None,
                    next_effort: None,
                },
                changed: false,
            })),
        }
    }

    #[cfg(test)]
    fn terminal(&self) -> Option<CellExit> {
        let state = self.state.lock();
        match &state.lifecycle {
            DraftLifecycle::Finished { exit, .. } => Some(exit.clone()),
            _ => None,
        }
    }
}

impl InvocationCompletionSource for EmbeddedContextBinding {
    fn completion(&self, output: JobOutput) -> Option<ProviderCompletion> {
        let state = self.state.lock();
        let DraftLifecycle::Finished { exit, draft } = &state.lifecycle else {
            return None;
        };
        // Native finalization seals this exact exit before delivering the
        // owner reply. Both result waiters therefore read the same draft and
        // eligibility even when cancellation observes the reply first.
        let output = match output {
            JobOutput::Completed(receipt)
                if exit.cause == exomonad_actor::CellExitCause::Cancelled =>
            {
                if exit.cleanup_confirmed {
                    JobOutput::CancelledWithReceipt(receipt)
                } else {
                    JobOutput::CancellationUnconfirmed("native cell cleanup unconfirmed".into())
                }
            }
            output => output,
        };
        let full_success =
            exit.permits_context_commit() && matches!(&output, JobOutput::Completed(Ok(_)));
        let context =
            if full_success && state.changed && matches!(draft, DraftEligibility::Retained) {
                ContextDisposition::Draft(state.draft.clone())
            } else {
                ContextDisposition::Unedited
            };
        Some(ProviderCompletion::provider(output, full_success, context))
    }
}

impl HostedContextBinding for EmbeddedContextBinding {
    fn admit(
        &self,
        execution: &WorkbenchExecutionId,
        invocation: &ToolInvocationContext,
        principal: PrincipalId,
    ) -> Result<(), EffectError> {
        let mut state = self.state.lock();
        if invocation != &self.invocation || !matches!(state.lifecycle, DraftLifecycle::Waiting) {
            return Err(EffectError::Handler(
                "context authority does not admit this invocation".into(),
            ));
        }
        state.lifecycle = DraftLifecycle::Active {
            execution: execution.clone(),
            principal,
        };
        Ok(())
    }

    fn prepare(
        &self,
        request: ContextReq,
        principal: PrincipalId,
        _: DataConTable,
    ) -> DeferredEffect {
        let state = self.state.clone();
        let model = self.model.clone();
        DeferredEffect::blocking(move || {
            let mut state = state.lock();
            match &state.lifecycle {
                DraftLifecycle::Active {
                    principal: owner, ..
                } if *owner == principal => {}
                _ => {
                    return Err(EffectError::Handler(
                        "context authority is closed or belongs to another principal".into(),
                    ))
                }
            }
            match request {
                ContextReq::GetContextWith => Ok(Response::new(super::context_wire::to_wire(
                    &state.draft.document,
                ))),
                ContextReq::PutContextWith(document) => {
                    state.draft.document = super::context_wire::from_wire(document)
                        .map_err(|error| EffectError::Handler(error.to_string()))?;
                    state.changed = true;
                    Ok(Response::new(()))
                }
                ContextReq::SetNextModelWith(selection) => {
                    let resolved = model(&selection).map_err(EffectError::Handler)?;
                    if resolved.trim().is_empty() {
                        return Err(EffectError::Handler("next model must be nonempty".into()));
                    }
                    state.draft.next_model = Some(resolved);
                    state.changed = true;
                    Ok(Response::new(()))
                }
                ContextReq::SetNextEffortWith(effort) => {
                    state.draft.next_effort = Some(match effort {
                        exomonad_actor::ForkEffort::Low => Effort::Low,
                        exomonad_actor::ForkEffort::Medium => Effort::Medium,
                        exomonad_actor::ForkEffort::High => Effort::High,
                    });
                    state.changed = true;
                    Ok(Response::new(()))
                }
            }
        })
    }

    fn cancel(&self) {
        let mut state = self.state.lock();
        // Close draft authority immediately while retaining the admitted
        // execution that may still report its exact cleanup outcome.
        let execution = match &state.lifecycle {
            DraftLifecycle::Waiting => None,
            DraftLifecycle::Active { execution, .. } => Some(execution.clone()),
            DraftLifecycle::Finished { .. } | DraftLifecycle::Cancelled { .. } => return,
        };
        state.lifecycle = DraftLifecycle::Cancelled { execution };
    }

    fn finish(&self, exit: CellExit) {
        let mut state = self.state.lock();
        let (execution, draft) = match &state.lifecycle {
            DraftLifecycle::Active { execution, .. } => (execution, DraftEligibility::Retained),
            DraftLifecycle::Cancelled {
                execution: Some(execution),
            } => (execution, DraftEligibility::Revoked),
            _ => return,
        };
        if execution == &exit.execution {
            state.lifecycle = DraftLifecycle::Finished { exit, draft };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exomonad_actor::CellExitCause;
    use exomonad_tool::{ConversationOrigin, OriginalOperation, ToolInvocationOrigin};

    fn binding() -> EmbeddedContextBinding {
        EmbeddedContextBinding {
            invocation: ToolInvocationContext {
                origin: ToolInvocationOrigin::Model(OriginalOperation {
                    origin: ConversationOrigin::External {
                        thread_id: "thread".into(),
                    },
                    request_id: "request".into(),
                    call_id: "call".into(),
                }),
                call_id: "call".into(),
                namespace: None,
            },
            model: Arc::new(|model| Ok(model.into())),
            state: Arc::new(Mutex::new(DraftState {
                lifecycle: DraftLifecycle::Waiting,
                draft: ContextDraft {
                    document: harness::context::ContextDocument { blocks: Vec::new() },
                    next_model: Some("staged-model".into()),
                    next_effort: None,
                },
                changed: true,
            })),
        }
    }

    fn prepare_result(
        binding: &EmbeddedContextBinding,
        request: ContextReq,
        principal: PrincipalId,
    ) -> Result<Response, EffectError> {
        let DeferredEffect::Blocking(work) =
            binding.prepare(request, principal, DataConTable::new())
        else {
            panic!("context service must retain its blocking request owner");
        };
        work.into_inner().unwrap()()
    }

    #[test]
    fn cancelled_context_grant_retains_only_its_matching_terminal_receipt() {
        let binding = binding();
        let execution = WorkbenchExecutionId::from_digest([3; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        binding.cancel();
        assert!(prepare_result(&binding, ContextReq::GetContextWith, principal).is_err());
        let exit = CellExit {
            execution,
            cause: CellExitCause::Cancelled,
            cleanup_confirmed: true,
        };
        binding.finish(CellExit {
            execution: WorkbenchExecutionId::from_digest([4; 16]),
            ..exit.clone()
        });
        assert!(binding.terminal().is_none());
        binding.finish(exit.clone());
        assert_eq!(binding.terminal(), Some(exit));
        let completion = binding
            .completion(JobOutput::Completed(Ok(serde_json::json!(true))))
            .unwrap();
        let disposition = completion.context;
        assert!(matches!(disposition, ContextDisposition::Unedited));
        assert!(prepare_result(
            &binding,
            ContextReq::SetNextModelWith("late".into()),
            principal
        )
        .is_err());
    }

    #[test]
    fn revoked_draft_stays_revoked_when_native_publication_wins_cancellation() {
        let binding = binding();
        let execution = WorkbenchExecutionId::from_digest([6; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        prepare_result(
            &binding,
            ContextReq::SetNextModelWith("updated-model".into()),
            principal,
        )
        .unwrap();
        binding.cancel();
        let exit = CellExit {
            execution,
            cause: CellExitCause::FullReturn,
            cleanup_confirmed: true,
        };
        binding.finish(exit.clone());
        assert_eq!(binding.terminal(), Some(exit));
        assert!(prepare_result(&binding, ContextReq::GetContextWith, principal).is_err());
        let output = JobOutput::Completed(Ok(serde_json::json!({"published": true})));
        for _ in 0..2 {
            let completion = binding.completion(output.clone()).unwrap();
            assert_eq!(completion.output, output);
            assert!(completion.full_success);
            assert_eq!(completion.context, ContextDisposition::Unedited);
        }
    }

    #[test]
    fn cancellation_after_full_return_preserves_the_committable_owner_receipt() {
        let binding = binding();
        let execution = WorkbenchExecutionId::from_digest([5; 16]);
        binding
            .admit(&execution, &binding.invocation, PrincipalId::new(1, 1))
            .unwrap();
        let exit = CellExit {
            execution,
            cause: CellExitCause::FullReturn,
            cleanup_confirmed: true,
        };
        binding.finish(exit.clone());
        binding.cancel();
        assert_eq!(binding.terminal(), Some(exit));
        let completion = binding
            .completion(JobOutput::Completed(Ok(serde_json::json!(true))))
            .unwrap();
        let disposition = completion.context;
        assert!(matches!(disposition, ContextDisposition::Draft(_)));
    }

    #[test]
    fn completion_source_exposes_sealed_draft_before_the_provider_waiter() {
        let binding = Arc::new(binding());
        let source: Arc<dyn InvocationCompletionSource> = binding.clone();
        let output = JobOutput::Completed(Ok(serde_json::json!({"exact": "owner receipt"})));
        assert!(source.completion(output.clone()).is_none());
        let execution = WorkbenchExecutionId::from_digest([8; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        assert!(source.completion(output.clone()).is_none());
        prepare_result(
            &binding,
            ContextReq::SetNextModelWith("resolved-model".into()),
            principal,
        )
        .unwrap();
        binding.finish(CellExit {
            execution: execution.clone(),
            cause: CellExitCause::FullReturn,
            cleanup_confirmed: true,
        });
        // The cancellation owner has the native receipt before the ordinary
        // provider future resumes; no result waiter populates this metadata.
        let owner = source.completion(output.clone()).unwrap();
        assert!(owner.full_success);
        assert_eq!(owner.output, output);
        let ContextDisposition::Draft(draft) = &owner.context else {
            panic!("sealed draft required");
        };
        assert_eq!(draft.next_model.as_deref(), Some("resolved-model"));
        binding.cancel();
        binding.finish(CellExit {
            execution,
            cause: CellExitCause::Cancelled,
            cleanup_confirmed: true,
        });
        assert!(prepare_result(
            &binding,
            ContextReq::SetNextModelWith("late".into()),
            principal
        )
        .is_err());
        assert_eq!(source.completion(output), Some(owner));
    }

    #[test]
    fn next_effort_is_staged_and_committed_with_a_successful_cell() {
        let binding = binding();
        let execution = WorkbenchExecutionId::from_digest([11; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        prepare_result(
            &binding,
            ContextReq::SetNextEffortWith(exomonad_actor::ForkEffort::High),
            principal,
        )
        .unwrap();
        binding.finish(CellExit {
            execution,
            cause: CellExitCause::FullReturn,
            cleanup_confirmed: true,
        });

        let completion = binding
            .completion(JobOutput::Completed(Ok(serde_json::json!(true))))
            .unwrap();
        let ContextDisposition::Draft(draft) = completion.context else {
            panic!("successful effort selection must produce a draft");
        };
        assert_eq!(draft.next_effort, Some(Effort::High));
    }

    #[test]
    fn next_effort_is_rolled_back_after_failure_or_cancellation() {
        for cause in [CellExitCause::Failed, CellExitCause::Cancelled] {
            let binding = binding();
            let execution = WorkbenchExecutionId::from_digest([12; 16]);
            let principal = PrincipalId::new(1, 1);
            binding
                .admit(&execution, &binding.invocation, principal)
                .unwrap();
            prepare_result(
                &binding,
                ContextReq::SetNextEffortWith(exomonad_actor::ForkEffort::High),
                principal,
            )
            .unwrap();
            if cause == CellExitCause::Cancelled {
                binding.cancel();
            }
            binding.finish(CellExit {
                execution,
                cause,
                cleanup_confirmed: true,
            });

            let completion = binding
                .completion(JobOutput::Completed(Ok(serde_json::json!(true))))
                .unwrap();
            assert_eq!(completion.context, ContextDisposition::Unedited);
        }
    }

    #[test]
    fn completion_source_refuses_drafts_for_partial_failed_or_unclean_cells() {
        for (cause, cleanup_confirmed) in [
            (CellExitCause::ReplyTransfer, true),
            (CellExitCause::Backgrounded, true),
            (CellExitCause::Rejected, true),
            (CellExitCause::Failed, true),
            (CellExitCause::FullReturn, false),
        ] {
            let binding = binding();
            let execution = WorkbenchExecutionId::from_digest([9; 16]);
            binding
                .admit(&execution, &binding.invocation, PrincipalId::new(1, 1))
                .unwrap();
            binding.finish(CellExit {
                execution,
                cause,
                cleanup_confirmed,
            });
            let output = JobOutput::Completed(Ok(serde_json::json!({"rendered": "success"})));
            let completion = binding.completion(output.clone()).unwrap();
            assert_eq!(completion.output, output);
            assert!(!completion.full_success);
            assert_eq!(completion.context, ContextDisposition::Unedited);
        }
    }

    #[test]
    fn completion_source_preserves_cancelled_receipts_and_denies_failed_results() {
        for cleanup_confirmed in [true, false] {
            let binding = binding();
            let execution = WorkbenchExecutionId::from_digest([10; 16]);
            binding
                .admit(&execution, &binding.invocation, PrincipalId::new(1, 1))
                .unwrap();
            binding.cancel();
            assert!(binding
                .completion(JobOutput::Completed(Ok(serde_json::json!(true))))
                .is_none());
            binding.finish(CellExit {
                execution,
                cause: CellExitCause::Cancelled,
                cleanup_confirmed,
            });
            let receipt = Err("exact retained prefix error".into());
            let completion = binding
                .completion(JobOutput::Completed(receipt.clone()))
                .unwrap();
            if cleanup_confirmed {
                assert_eq!(completion.output, JobOutput::CancelledWithReceipt(receipt));
            } else {
                assert!(matches!(
                    completion.output,
                    JobOutput::CancellationUnconfirmed(_)
                ));
            }
            assert!(!completion.full_success);
            assert_eq!(completion.context, ContextDisposition::Unedited);
        }
        let binding = binding();
        let execution = WorkbenchExecutionId::from_digest([11; 16]);
        binding
            .admit(&execution, &binding.invocation, PrincipalId::new(1, 1))
            .unwrap();
        binding.finish(CellExit {
            execution,
            cause: CellExitCause::FullReturn,
            cleanup_confirmed: true,
        });
        for output in [
            JobOutput::Completed(Err("exact failure".into())),
            JobOutput::Cancelled,
            JobOutput::Interrupted,
        ] {
            let completion = binding.completion(output.clone()).unwrap();
            assert_eq!(completion.output, output);
            assert!(!completion.full_success);
            assert_eq!(completion.context, ContextDisposition::Unedited);
        }
    }
}
