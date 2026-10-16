//! Per-invocation draft authority; durable publication belongs to the harness Store.
use std::sync::Arc;

use exomonad_actor::{CellExit, ContextReq, HostedContextBinding};
use exomonad_tool::ToolInvocationContext;
use harness::{
    context::{
        ContextDraft, ContextPatch, ContextProblem, ContextProblemCode, ContextReference,
        ContextSnapshot, ContextTextSelector, ContextTransaction, ContextTransactionDraft,
        ContextTransactionStatus,
    },
    model::Effort,
    provider::{ContextDisposition, InvocationCompletionSource, ProviderCompletion},
    turn::JobOutput,
};
use parking_lot::Mutex;
use tidepool_effect::{error::EffectError, DeferredEffect, Response};
use tidepool_repr::{DataConTable, PrincipalId};
use tidepool_runtime::session::WorkbenchExecutionId;

pub(super) type InitialContextObserver = Arc<
    dyn Fn() -> Result<tidepool_bridge_effects::InitialContextObservation, String> + Send + Sync,
>;

pub(super) type ModelResolver = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

type TransactionIssuer =
    Arc<dyn Fn(&ContextPatch) -> Result<ContextTransaction, String> + Send + Sync>;
type TransactionObserver =
    Arc<dyn Fn(&ContextTransaction) -> Result<ContextTransactionStatus, String> + Send + Sync>;
type OriginalTextObserver = Arc<
    dyn Fn(
            &ContextReference,
            &ContextTextSelector,
        ) -> Result<Result<String, ContextProblem>, String>
        + Send
        + Sync,
>;

pub(super) struct EmbeddedContextBinding {
    invocation: ToolInvocationContext,
    model: ModelResolver,
    initial_context: InitialContextObserver,
    transaction_issuer: TransactionIssuer,
    transaction_observer: TransactionObserver,
    original_text: OriginalTextObserver,
    state: Arc<Mutex<DraftState>>,
    request_reply: Option<EmbeddedRequestReply>,
}

struct EmbeddedRequestReply {
    activation: Arc<exomonad_actor::RequestReplyActivation>,
    admission: harness::provider::InvocationAdmission,
    identity: harness::embedding::HostIdentity,
    terminal: Mutex<Option<harness::provider::InvocationTerminal>>,
}

struct DraftState {
    lifecycle: DraftLifecycle,
    draft: ContextDraft,
    changed: bool,
    staged: Option<(ContextPatch, ContextTransaction)>,
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
        initial_context: InitialContextObserver,
        store: Arc<harness::store::Store>,
    ) -> Self {
        let issuing_snapshot = Arc::new(snapshot.clone());
        let original_snapshot = issuing_snapshot.clone();
        let issuing_store = store.clone();
        let transaction_issuer = Arc::new(move |patch: &ContextPatch| {
            issuing_store
                .context_transaction(&issuing_snapshot, patch)
                .map_err(|error| error.to_string())
        });
        let identity = snapshot.operation.origin.clone();
        let observing_store = store.clone();
        let transaction_observer = Arc::new(move |transaction: &ContextTransaction| {
            observing_store
                .context_transaction_status(&identity, transaction)
                .map_err(|error| error.to_string())
        });
        let original_text = Arc::new(
            move |reference: &ContextReference, selector: &ContextTextSelector| {
                store
                    .original_context_text(&original_snapshot, reference, selector)
                    .map_err(|error| error.to_string())
            },
        );
        Self {
            invocation,
            model,
            initial_context,
            transaction_issuer,
            transaction_observer,
            original_text,
            request_reply: None,
            state: Arc::new(Mutex::new(DraftState {
                lifecycle: DraftLifecycle::Waiting,
                draft: ContextDraft {
                    document: snapshot.document.clone(),
                    next_model: None,
                    next_effort: None,
                },
                changed: false,
                staged: None,
            })),
        }
    }

    pub(super) fn with_request_reply(
        mut self,
        activation: Arc<exomonad_actor::RequestReplyActivation>,
        admission: Option<harness::provider::InvocationAdmission>,
        identity: harness::embedding::HostIdentity,
    ) -> Result<Self, String> {
        let admission = admission.ok_or("request-scoped call has no scheduler admission")?;
        let scope = activation.host_attachment(harness::provider::RequestActivation::new)?;
        if admission.activation() != scope.as_ref() {
            return Err("call admission belongs to a different request activation".into());
        }
        self.request_reply = Some(EmbeddedRequestReply {
            activation,
            admission,
            identity,
            terminal: Mutex::new(None),
        });
        Ok(self)
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
        let context = if let Some((patch, _)) = &state.staged {
            let transaction = ContextTransactionDraft {
                patch: patch.clone(),
                next_model: state.draft.next_model.clone(),
                next_effort: state.draft.next_effort,
            };
            if full_success && matches!(draft, DraftEligibility::Retained) {
                ContextDisposition::Transaction(transaction)
            } else {
                ContextDisposition::Discarded(transaction)
            }
        } else if full_success && state.changed && matches!(draft, DraftEligibility::Retained) {
            ContextDisposition::Draft(state.draft.clone())
        } else {
            ContextDisposition::Unedited
        };
        Some(ProviderCompletion {
            activation_terminal: self
                .request_reply
                .as_ref()
                .and_then(|reply| reply.terminal.lock().clone()),
            output,
            full_success,
            context,
        })
    }
}

impl HostedContextBinding for EmbeddedContextBinding {
    fn admit_request_reply(
        &self,
        activation: &exomonad_actor::RequestReplyActivation,
        claim: &mut dyn FnMut(
            &[exomonad_actor::HostedPendingInvocation],
        ) -> Result<(), exomonad_actor::RequestReplyAdmissionError>,
    ) -> Result<(), exomonad_actor::RequestReplyAdmissionError> {
        use exomonad_actor::{
            HostedInvocationState as NativeState, HostedPendingInvocation,
            RequestReplyAdmissionError as Error,
        };
        use harness::provider::{InvocationAdmissionError, InvocationState};
        let reply = self.request_reply.as_ref().ok_or_else(|| {
            Error::Unavailable("invocation has no request-bound scheduler admission".into())
        })?;
        if !std::ptr::eq(activation, reply.activation.as_ref()) {
            return Err(Error::Refused(exomonad_actor::ReplyError::Stale));
        }
        match reply.admission.try_admit_terminal(|pending| {
            let pending = pending
                .iter()
                .map(|operation| {
                    let invocation = super::embedded_harness::embedded_invocation_context(
                        &reply.identity,
                        &operation.operation,
                    )
                    .map_err(|error| Error::Unavailable(error.to_string()))?;
                    Ok(HostedPendingInvocation {
                        invocation,
                        name: operation.name.clone(),
                        state: match operation.state {
                            InvocationState::Queued => NativeState::Queued,
                            InvocationState::Running => NativeState::Running,
                            InvocationState::Unconfirmed => NativeState::Unconfirmed,
                        },
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            claim(&pending)
        }) {
            Ok(((), terminal)) => {
                *reply.terminal.lock() = Some(terminal);
                Ok(())
            }
            Err(InvocationAdmissionError::Claim(error)) => Err(error),
            Err(InvocationAdmissionError::AlreadyTerminated) => {
                Err(Error::Refused(exomonad_actor::ReplyError::AlreadySettled))
            }
            Err(InvocationAdmissionError::ForeignInvocation) => Err(Error::Refused(exomonad_actor::ReplyError::Stale)),
            Err(InvocationAdmissionError::NotSettlement) => Err(Error::Refused(exomonad_actor::ReplyError::Unauthorized)),
            Err(InvocationAdmissionError::Unavailable) => Err(Error::Unavailable("scheduler admission is currently unavailable; retry without assuming an empty pending inventory".into())),
        }
    }

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
        let shared_state = self.state.clone();
        let model = self.model.clone();
        let initial_context = self.initial_context.clone();
        let transaction_issuer = self.transaction_issuer.clone();
        let transaction_observer = self.transaction_observer.clone();
        let original_text = self.original_text.clone();
        DeferredEffect::blocking(move || {
            let mut state = shared_state.lock();
            match &state.lifecycle {
                DraftLifecycle::Active {
                    principal: owner, ..
                } if *owner == principal => {}
                _ => {
                    return Err(EffectError::Handler(
                        "context authority is closed or belongs to another principal".into(),
                    ));
                }
            }
            match request {
                ContextReq::GetInitialContextWith => initial_context()
                    .map(Response::new)
                    .map_err(EffectError::Handler),
                ContextReq::GetContextWith => {
                    Ok(Response::new(super::context_wire::inspection_to_wire(
                        &harness::context::inspect_context(&state.draft.document),
                    )))
                }
                ContextReq::StageContextWith(patch) => {
                    let patch = match super::context_wire::patch_from_wire(patch) {
                        Ok(patch) => patch,
                        Err(error) => {
                            return Ok(Response::new(super::context_wire::stage_refused(vec![
                                ContextProblem {
                                    code: ContextProblemCode::InvalidPatch,
                                    reference: None,
                                    detail: error.to_string(),
                                },
                            ])))
                        }
                    };
                    if let Some((existing, transaction)) = &state.staged {
                        return Ok(Response::new(if existing == &patch {
                            super::context_wire::stage_accepted(transaction)
                        } else {
                            super::context_wire::stage_refused(vec![ContextProblem {
                                code: ContextProblemCode::AlreadyStaged,
                                reference: None,
                                detail: "this invocation already staged a different context batch"
                                    .into(),
                            }])
                        }));
                    }
                    if let Err(problems) =
                        harness::context::check_context_patch(&state.draft.document, &patch)
                    {
                        return Ok(Response::new(super::context_wire::stage_refused(problems)));
                    }
                    drop(state);
                    let transaction = transaction_issuer(&patch).map_err(EffectError::Handler)?;
                    let mut state = shared_state.lock();
                    if !matches!(state.lifecycle, DraftLifecycle::Active { principal: owner, .. } if owner == principal)
                    {
                        return Err(EffectError::Handler(
                            "context authority closed while preparing its batch".into(),
                        ));
                    }
                    if let Some((existing, existing_transaction)) = &state.staged {
                        return Ok(Response::new(if existing == &patch {
                            super::context_wire::stage_accepted(existing_transaction)
                        } else {
                            super::context_wire::stage_refused(vec![ContextProblem {
                                code: ContextProblemCode::AlreadyStaged,
                                reference: None,
                                detail: "this invocation already staged a different context batch"
                                    .into(),
                            }])
                        }));
                    }
                    state.staged = Some((patch, transaction.clone()));
                    Ok(Response::new(super::context_wire::stage_accepted(
                        &transaction,
                    )))
                }
                ContextReq::InspectContextTransactionWith(transaction) => {
                    let transaction = super::context_wire::transaction_from_wire(transaction);
                    if state
                        .staged
                        .as_ref()
                        .is_some_and(|(_, staged)| staged == &transaction)
                    {
                        return Ok(Response::new(super::context_wire::status_to_wire(
                            ContextTransactionStatus::Staged,
                        )?));
                    }
                    drop(state);
                    let status =
                        transaction_observer(&transaction).map_err(EffectError::Handler)?;
                    Ok(Response::new(super::context_wire::status_to_wire(status)?))
                }
                ContextReq::InspectOriginalTextWith(reference, selector) => {
                    let reference = super::context_wire::reference_from_wire(reference);
                    let selector = match super::context_wire::selector_from_wire(selector) {
                        Ok(selector) => selector,
                        Err(error) => return Ok(Response::new(tidepool_bridge_effects::OriginalTextObservation::OriginalTextUnsupported(error.to_string()))),
                    };
                    drop(state);
                    Ok(Response::new(match original_text(&reference, &selector) {
                        Ok(observed) => super::context_wire::original_to_wire(observed),
                        Err(detail) => tidepool_bridge_effects::OriginalTextObservation::OriginalTextUnavailable(detail),
                    }))
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
            request_reply: None,
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
            initial_context: Arc::new(|| {
                Ok(tidepool_bridge_effects::InitialContextObservation::InitialContextAbsent)
            }),
            transaction_issuer: Arc::new(|patch| {
                Ok(ContextTransaction {
                    raw: serde_json::to_string(patch).unwrap(),
                })
            }),
            transaction_observer: Arc::new(|_| {
                Ok(ContextTransactionStatus::Unknown("not retained".into()))
            }),
            original_text: Arc::new(|_, _| {
                Ok(Err(ContextProblem {
                    code: ContextProblemCode::Missing,
                    reference: None,
                    detail: "not retained".into(),
                }))
            }),
            state: Arc::new(Mutex::new(DraftState {
                lifecycle: DraftLifecycle::Waiting,
                draft: ContextDraft {
                    document: harness::context::ContextDocument { blocks: Vec::new() },
                    next_model: Some("staged-model".into()),
                    next_effort: None,
                },
                changed: true,
                staged: None,
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

    fn note_patch(text: &str) -> ContextReq {
        ContextReq::StageContextWith(tidepool_bridge_effects::ContextPatch {
            changes: vec![tidepool_bridge_effects::ContextChange::AppendContextNote {
                text: text.into(),
                sources: vec![],
            }],
        })
    }

    #[test]
    fn one_batch_stages_idempotently_and_self_inspection_never_waits_for_publication() {
        let mut binding = binding();
        let shared_state = binding.state.clone();
        binding.transaction_issuer = Arc::new(move |patch| {
            assert!(
                shared_state.try_lock().is_some(),
                "issuance must run outside draft lock"
            );
            Ok(ContextTransaction {
                raw: serde_json::to_string(patch).unwrap(),
            })
        });
        binding.transaction_observer =
            Arc::new(|_| panic!("own staged transaction must not query Store"));
        let execution = WorkbenchExecutionId::from_digest([21; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        prepare_result(&binding, note_patch("first"), principal).unwrap();
        let staged = binding.state.lock().staged.clone().unwrap();
        prepare_result(&binding, note_patch("first"), principal).unwrap();
        assert_eq!(binding.state.lock().staged, Some(staged.clone()));
        // Refusal is ordinary returned data; it cannot replace the first batch.
        prepare_result(&binding, note_patch("second"), principal).unwrap();
        assert_eq!(binding.state.lock().staged, Some(staged.clone()));
        prepare_result(
            &binding,
            ContextReq::InspectContextTransactionWith(
                tidepool_bridge_effects::ContextTransaction { raw: staged.1.raw },
            ),
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
        let ContextDisposition::Transaction(transaction) = completion.context else {
            panic!("successful full invocation must retain the exact staged batch");
        };
        assert_eq!(transaction.patch, staged.0);
        assert_eq!(transaction.next_model.as_deref(), Some("staged-model"));
        assert_eq!(transaction.next_effort, None);
    }

    #[test]
    fn failed_and_cancelled_cells_retain_staged_association_for_store_reconciliation() {
        for cause in [CellExitCause::Failed, CellExitCause::Cancelled] {
            let binding = binding();
            let execution = WorkbenchExecutionId::from_digest([22; 16]);
            let principal = PrincipalId::new(1, 1);
            binding
                .admit(&execution, &binding.invocation, principal)
                .unwrap();
            prepare_result(&binding, note_patch("retained identity"), principal).unwrap();
            let patch = binding.state.lock().staged.as_ref().unwrap().0.clone();
            if cause == CellExitCause::Cancelled {
                binding.cancel();
            }
            binding.finish(CellExit {
                execution,
                cause,
                cleanup_confirmed: true,
            });
            let completion = binding
                .completion(JobOutput::Completed(Err("later statement failed".into())))
                .unwrap();
            assert!(!completion.full_success);
            let ContextDisposition::Discarded(discarded) = completion.context else {
                panic!(
                    "discarding mutation must preserve its association for terminal observation"
                );
            };
            assert_eq!(discarded.patch, patch);
        }
    }

    #[test]
    fn observation_callbacks_run_without_holding_invocation_draft_lock() {
        let mut binding = binding();
        let shared_state = binding.state.clone();
        binding.transaction_observer = Arc::new(move |_| {
            assert!(shared_state.try_lock().is_some());
            Ok(ContextTransactionStatus::Unknown(
                "no terminal evidence".into(),
            ))
        });
        let shared_state = binding.state.clone();
        binding.original_text = Arc::new(move |_, _| {
            assert!(shared_state.try_lock().is_some());
            Ok(Ok("original".into()))
        });
        let execution = WorkbenchExecutionId::from_digest([23; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        prepare_result(
            &binding,
            ContextReq::InspectContextTransactionWith(
                tidepool_bridge_effects::ContextTransaction {
                    raw: "prior-operation".into(),
                },
            ),
            principal,
        )
        .unwrap();
        prepare_result(
            &binding,
            ContextReq::InspectOriginalTextWith(
                tidepool_bridge_effects::ContextReference {
                    raw: "prior-text".into(),
                },
                tidepool_bridge_effects::ContextTextSelector::MessageText { part: 0 },
            ),
            principal,
        )
        .unwrap();
    }

    #[test]
    fn startup_observation_checks_current_authority_and_never_stages_edits() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut binding = binding();
        let observations = Arc::new(AtomicUsize::new(0));
        let counted = observations.clone();
        binding.initial_context = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(tidepool_bridge_effects::InitialContextObservation::InitialContextAbsent)
        });
        binding.state.lock().changed = false;
        let execution = WorkbenchExecutionId::from_digest([13; 16]);
        let principal = PrincipalId::new(1, 1);
        assert!(prepare_result(&binding, ContextReq::GetInitialContextWith, principal).is_err());
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        assert!(prepare_result(
            &binding,
            ContextReq::GetInitialContextWith,
            PrincipalId::new(2, 1)
        )
        .is_err());
        for _ in 0..2 {
            assert!(prepare_result(&binding, ContextReq::GetInitialContextWith, principal).is_ok());
        }
        assert_eq!(observations.load(Ordering::SeqCst), 2);
        binding.finish(CellExit {
            execution,
            cause: CellExitCause::FullReturn,
            cleanup_confirmed: true,
        });
        assert!(matches!(
            binding
                .completion(JobOutput::Completed(Ok(serde_json::json!(true))))
                .unwrap()
                .context,
            ContextDisposition::Unedited
        ));
        assert!(prepare_result(&binding, ContextReq::GetInitialContextWith, principal).is_err());
        assert_eq!(observations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn unreadable_startup_receipt_fails_without_claiming_an_absent_hook() {
        let mut binding = binding();
        binding.initial_context = Arc::new(|| Err("startup journal unavailable".into()));
        let execution = WorkbenchExecutionId::from_digest([14; 16]);
        let principal = PrincipalId::new(1, 1);
        binding
            .admit(&execution, &binding.invocation, principal)
            .unwrap();
        assert!(
            matches!(prepare_result(&binding, ContextReq::GetInitialContextWith, principal),
            Err(EffectError::Handler(detail)) if detail == "startup journal unavailable")
        );
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
