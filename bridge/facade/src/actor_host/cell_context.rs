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
    Cancelled,
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

    pub(super) fn completion(&self) -> (bool, ContextDisposition) {
        let state = self.state.lock();
        let eligible = matches!(&state.lifecycle, DraftLifecycle::Finished(exit) if exit.permits_context_commit());
        let disposition = if eligible && state.changed {
            ContextDisposition::Draft(state.draft.clone())
        } else {
            ContextDisposition::Unedited
        };
        (eligible, disposition)
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
        // A completed invocation's receipt must survive cancellation racing the waiter.
        if !matches!(state.lifecycle, DraftLifecycle::Finished(_)) {
            state.lifecycle = DraftLifecycle::Cancelled;
        }
    }

    fn finish(&self, exit: CellExit) {
        let mut state = self.state.lock();
        if matches!(&state.lifecycle, DraftLifecycle::Active { execution, .. } if execution == &exit.execution)
        {
            state.lifecycle = DraftLifecycle::Finished(exit);
        }
    }
}
