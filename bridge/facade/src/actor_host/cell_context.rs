//! Per-invocation draft authority; durable publication belongs to the harness Store.
use std::sync::Arc;

use exomonad_actor::{CellExit, ContextReq, HostedContextBinding};
use exomonad_tool::ToolInvocationContext;
use harness::{
    context::{ContextDraft, ContextSnapshot},
    provider::ContextDisposition,
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
    Finished(CellExit),
    Cancelled {
        execution: Option<WorkbenchExecutionId>,
    },
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
                },
                changed: false,
            })),
        }
    }

    pub(super) fn completion(&self) -> (Option<CellExit>, ContextDisposition) {
        let state = self.state.lock();
        let exit = match &state.lifecycle {
            DraftLifecycle::Finished(exit) => Some(exit.clone()),
            _ => None,
        };
        let eligible = exit.as_ref().is_some_and(CellExit::permits_context_commit);
        let disposition = if eligible && state.changed {
            ContextDisposition::Draft(state.draft.clone())
        } else {
            ContextDisposition::Unedited
        };
        (exit, disposition)
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
                    state.draft.document = super::context_wire::from_wire(document);
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
            DraftLifecycle::Finished(_) | DraftLifecycle::Cancelled { .. } => return,
        };
        state.lifecycle = DraftLifecycle::Cancelled { execution };
    }

    fn finish(&self, exit: CellExit) {
        let mut state = self.state.lock();
        let admitted = match &state.lifecycle {
            DraftLifecycle::Active { execution, .. }
            | DraftLifecycle::Cancelled {
                execution: Some(execution),
            } => execution == &exit.execution,
            _ => false,
        };
        if admitted {
            state.lifecycle = DraftLifecycle::Finished(exit);
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
        assert!(binding.completion().0.is_none());
        binding.finish(exit.clone());
        let (retained, disposition) = binding.completion();
        assert_eq!(retained, Some(exit));
        assert!(matches!(disposition, ContextDisposition::Unedited));
        assert!(prepare_result(
            &binding,
            ContextReq::SetNextModelWith("late".into()),
            principal
        )
        .is_err());
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
        let (retained, disposition) = binding.completion();
        assert_eq!(retained, Some(exit));
        assert!(matches!(disposition, ContextDisposition::Draft(_)));
    }
}
