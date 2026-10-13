//! Exclusive tool protocol around the shared native invocation cursor.
use super::green::{GreenAdvance, InvocationExecution};
use super::*;
use tidepool_bridge_effects::{CleanupError, ScopeFailure};

#[derive(Default)]
pub(super) struct ToolEffectCompletion {
    observed_child: Option<ActorRef>,
}

pub(super) struct ToolCursor {
    execution: InvocationExecution<ToolEffectCompletion>,
    scopes: Vec<scopes::ScopeFrame>,
    work: Arc<InvocationWork>,
    control: Arc<crate::WorkbenchExecutionControl>,
    publication: CheckpointPublication,
    issued_snapshot: Option<crate::resident_workbench::ActorRequestSnapshot>,
}

impl ToolCursor {
    pub(super) fn new(
        work: Arc<InvocationWork>,
        control: Arc<crate::WorkbenchExecutionControl>,
        publication: CheckpointPublication,
        issued_snapshot: Option<crate::resident_workbench::ActorRequestSnapshot>,
    ) -> Self {
        Self {
            execution: Default::default(),
            scopes: Vec::new(),
            work,
            control,
            publication,
            issued_snapshot,
        }
    }

    pub(super) fn native<'a, H, O>(
        &'a mut self,
        environment: &'a ResidentEnvironment<H, O>,
        context: &ActorSessionContext,
    ) -> super::green::NativeExecution<'a, H, O, ToolEffectCompletion> {
        self.execution.native(environment, context)
    }
    pub(super) fn realm(&self, actor_realm: RealmId) -> RealmId {
        self.scopes
            .last()
            .map(|frame| frame.realm)
            .or_else(|| self.execution.active_realm())
            .unwrap_or(actor_realm)
    }

    pub(super) fn is_main_terminal(&self) -> bool {
        self.scopes.is_empty() && self.execution.active_work().is_none()
    }

    pub(super) fn check_admission(
        &mut self,
        kernel: &KernelContext,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let error = match kernel.requested_shutdown() {
            Some(terminal) => Some(ResidentActorWorkbenchError::RetiredBeforeAdmission(
                terminal,
            )),
            None if self
                .control
                .native_cancel()
                .load(std::sync::atomic::Ordering::Acquire) =>
            {
                Some(ResidentActorWorkbenchError::InvocationCancelled)
            }
            None => None,
        };
        if let Some(error) = error {
            self.work.close();
            self.execution.cancel_parent();
            return Err(error);
        }
        Ok(())
    }

    fn owner(&self) -> CurrentEffectOwner<'static> {
        let base = CurrentEffectOwner::Tool {
            issued_snapshot: self.issued_snapshot.clone(),
            work: self.work.clone(),
            publication: self.publication.clone(),
            control: self.control.clone(),
        };
        let scope = self
            .scopes
            .last()
            .map(|frame| frame.work.clone())
            .or_else(|| self.execution.active_work());
        match scope {
            Some(scope) => CurrentEffectOwner::Scoped {
                base: Box::new(base),
                scope,
                wait_control: self.execution.active_wait_control(),
            },
            None => base,
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) async fn advance_tool_frontier(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        cursor: &mut ToolCursor,
        boundary: ResidentActorBoundary,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        cursor.check_admission(kernel)?;
        let owner = cursor.owner();
        match boundary {
            ResidentActorBoundary::Green(boundary) => {
                match cursor.execution.prepare(
                    self,
                    kernel,
                    context,
                    &owner,
                    &mut cursor.scopes,
                    boundary,
                )? {
                    GreenAdvance::Start {
                        callback,
                        realm,
                        token,
                        work,
                    } => {
                        return cursor
                            .execution
                            .native(&self.environment, context)
                            .run_scope_callback(callback, realm, token, work)
                            .await;
                    }
                    GreenAdvance::Wait => {}
                }
            }
            ResidentActorBoundary::ScopeRun {
                continuation,
                callback,
            } => {
                let (work, realm) = match self.register_resource_scope(context, &owner) {
                    Ok(scope) => scope,
                    Err(detail) => {
                        return cursor
                            .execution
                            .native(&self.environment, context)
                            .resume_value(
                                continuation,
                                (
                                    Err::<(), _>(ScopeFailure::ScopeRejected(detail)),
                                    Ok::<(), CleanupError>(()),
                                ),
                            )
                            .await
                    }
                };
                let token = work.scope_token().expect("native lexical scope identity");
                cursor.scopes.push(scopes::ScopeFrame {
                    continuation,
                    work: work.clone(),
                    realm,
                });
                return cursor
                    .execution
                    .native(&self.environment, context)
                    .run_scope_callback(callback, realm, token, work)
                    .await;
            }
            ResidentActorBoundary::ScopeDone {
                token,
                continuation,
            } => {
                let Some(frame) = cursor.scopes.pop() else {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "scope completion outside its owning tool delimiter".into(),
                    ));
                };
                let body = if frame.work.scope_token() == Some(token) {
                    Ok(())
                } else {
                    Err(ScopeFailure::ScopeEvaluationFailed(
                        "scope completion belongs to a different tool delimiter".into(),
                    ))
                };
                drop(continuation);
                if !cursor.execution.has_green() {
                    return cursor
                        .execution
                        .native(&self.environment, context)
                        .finish_scope(kernel, frame, body)
                        .await;
                }
                let operation = scopes::finish_scope(
                    self.environment.clone(),
                    kernel.clone(),
                    context.clone(),
                    frame,
                    body,
                );
                cursor.execution.enqueue_frontier(
                    std::mem::take(&mut cursor.scopes),
                    Box::pin(async move { (operation.await, ToolEffectCompletion::default()) }),
                );
            }
            boundary if !cursor.execution.has_green() => {
                let environment = self.environment.clone();
                return cursor
                    .execution
                    .native(&environment, context)
                    .resolve_effect(
                        self,
                        kernel,
                        owner,
                        &crate::CallAncestry::begin(context.actor),
                        boundary,
                    )
                    .await
                    .outcome;
            }
            boundary => {
                let boundary = prepare_execution_effect(context, &owner, boundary);
                let (boundary, _) = cursor
                    .execution
                    .native(&self.environment, context)
                    .prepare_display(
                        boundary,
                        DEFAULT_DISPLAY_CHARACTER_ALLOWANCE,
                        None,
                        None,
                        crate::request::sources::CallbackExecution {
                            context: context.clone(),
                            snapshot: owner.issued_snapshot().cloned(),
                            scope: owner.callback_scope(),
                        },
                    )
                    .await?;
                match self
                    .prepare_invocation_wait(kernel, context, &owner, boundary)
                    .await?
                {
                    Ok(wait) => {
                        let observed_child = wait.observe_after_resume();
                        let control = owner.control().expect("tool frontier wait owner");
                        control.arm_sleep();
                        let work = owner
                            .ephemeral_work()
                            .expect("tool frontier resource owner");
                        let commands_permitted = self
                            .descriptor
                            .capabilities()
                            .effect_keys()
                            .contains(&crate::ActorEffectKey::Commands);
                        let operation = owned_workbench::await_effect(
                            self.environment.clone(),
                            kernel.clone(),
                            context.clone(),
                            control,
                            wait,
                            commands_permitted,
                            work,
                            owner.model(),
                        );
                        cursor.execution.enqueue_frontier(
                            std::mem::take(&mut cursor.scopes),
                            Box::pin(async move {
                                let result = operation.await;
                                let outcome = if result.retained_job_binding.is_some() {
                                    Err(ResidentActorWorkbenchError::ActorProtocol(
                                        "notebook command binding reached the tool frontier".into(),
                                    ))
                                } else {
                                    result.outcome
                                };
                                (outcome, ToolEffectCompletion { observed_child })
                            }),
                        );
                    }
                    Err(boundary) => {
                        // Actor protocol transitions stay under the original exclusive tool owner.
                        let environment = self.environment.clone();
                        return cursor
                            .execution
                            .native(&environment, context)
                            .resolve_effect(
                                self,
                                kernel,
                                owner,
                                &crate::CallAncestry::begin(context.actor),
                                boundary,
                            )
                            .await
                            .outcome;
                    }
                }
            }
        }
        self.next_tool_frontier(kernel, context, cursor).await
    }

    async fn next_tool_frontier(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        cursor: &mut ToolCursor,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        loop {
            cursor.check_admission(kernel)?;
            let retirement = kernel.retained_exit();
            let execution = &mut cursor.execution;
            let completion = tokio::select! {
                biased;
                () = cursor.control.wait_for_cancellation() => {
                    cursor.work.close(); execution.cancel_parent();
                    return Err(ResidentActorWorkbenchError::InvocationCancelled);
                }
                terminal = retirement.wait_requested_shutdown() => {
                    cursor.work.close(); execution.cancel_parent();
                    return Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(terminal));
                }
                completion = execution.next() => completion?,
            };
            if let Some(mut frontier) = execution.apply(self, context, completion)? {
                cursor.scopes = frontier.scopes;
                let result = frontier.result.take().expect("ready native tool frontier");
                if result.is_ok()
                    || matches!(&result, Err(ResidentActorWorkbenchError::Delivered(_)))
                {
                    if let Some(child) = frontier
                        .attachment
                        .and_then(|attachment| attachment.observed_child)
                    {
                        self.record_child_observation(child);
                    }
                }
                return result;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_owner_retains_original_invocation_and_interaction_control() {
        let actor = ActorRef {
            id: crate::ActorId(145),
            incarnation: crate::Incarnation::FIRST,
        };
        let control = crate::WorkbenchExecutionControl::untracked();
        let work = InvocationWork::new(actor, control.reservation_owner(actor).unwrap());
        let cursor = ToolCursor::new(
            work.clone(),
            control.clone(),
            CheckpointPublication::Resident,
            None,
        );
        let owner = cursor.owner();
        assert!(Arc::ptr_eq(&owner.invocation_work().unwrap(), &work));
        assert!(Arc::ptr_eq(&owner.ephemeral_work().unwrap(), &work));
        assert!(Arc::ptr_eq(&owner.interaction_control().unwrap(), &control));
        assert!(cursor.is_main_terminal());
        let child = work.new_scope().unwrap();
        let wait_control = crate::WorkbenchExecutionControl::untracked();
        let child_owner = CurrentEffectOwner::Scoped {
            base: Box::new(owner),
            scope: child.clone(),
            wait_control: Some(wait_control.clone()),
        };
        assert!(Arc::ptr_eq(&child_owner.control().unwrap(), &wait_control));
        assert!(Arc::ptr_eq(
            &child_owner.interaction_control().unwrap(),
            &control
        ));
        assert!(Arc::ptr_eq(&child_owner.invocation_work().unwrap(), &work));
        assert!(Arc::ptr_eq(&child_owner.ephemeral_work().unwrap(), &child));
        control.request_cancellation();
        assert!(child_owner
            .interaction_control()
            .unwrap()
            .cancellation_requested());
        assert!(!child_owner.control().unwrap().cancellation_requested());
    }
    #[tokio::test]
    async fn main_tool_reply_progress_polls_retained_fifo_frontier() {
        use tidepool_runtime::session::registry::{CheckoutRequest, SessionRegistry};
        let actor = ActorRef {
            id: crate::ActorId(146),
            incarnation: crate::Incarnation::FIRST,
        };
        let control = crate::WorkbenchExecutionControl::untracked();
        let work = InvocationWork::new(actor, control.reservation_owner(actor).unwrap());
        let mut cursor = ToolCursor::new(work, control, CheckpointPublication::Resident, None);
        let session = tidepool_repr::SessionId(993);
        let machines = Arc::new(SessionRegistry::<(), u8>::new());
        assert!(machines.insert_idle(session, Box::new(())).is_none());
        let (machine, held) = machines.checkout_run(session).unwrap().into_parts();
        let queued_machines = machines.clone();
        let mut green = super::green::GreenInvocation::default();
        green.enqueue_frontier(
            Vec::new(),
            Box::pin(async move {
                let (machine, receipt) = queued_machines
                    .checkout_queued(session, CheckoutRequest::Run)
                    .await
                    .unwrap()
                    .into_parts();
                queued_machines.settle_suspended(receipt, machine, Vec::new());
                (
                    Err(ResidentActorWorkbenchError::InvocationCancelled),
                    ToolEffectCompletion::default(),
                )
            }),
        );
        assert!(matches!(
            futures_util::poll!(Box::pin(green.next())),
            std::task::Poll::Pending
        ));
        cursor.execution = super::green::InvocationExecution::from_green(green);
        assert!(
            cursor.is_main_terminal(),
            "main terminal does not release retained frontiers"
        );
        let resume = machines.checkout_queued(session, CheckoutRequest::Run);
        tokio::pin!(resume);
        assert!(matches!(
            futures_util::poll!(&mut resume),
            std::task::Poll::Pending
        ));
        machines.settle_suspended(held, machine, Vec::new());
        let checkout = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            cursor.execution.test_drive(resume),
        )
        .await
        .expect("retained frontier releases FIFO before main reply continuation")
        .unwrap();
        let (machine, receipt) = checkout.into_parts();
        machines.settle_suspended(receipt, machine, Vec::new());
        let completion = cursor.execution.next().await.unwrap();
        assert!(matches!(
            completion.result,
            Some(Err(ResidentActorWorkbenchError::InvocationCancelled))
        ));
        assert!(completion.attachment.is_some());
    }
}
