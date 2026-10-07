//! Exclusive tool protocol around the shared native invocation cursor.
use super::green::{GreenAdvance, GreenInvocation};
use super::*;
use tidepool_bridge_effects::{CleanupError, ScopeFailure};

pub(super) struct ToolCursor {
    green: Option<GreenInvocation<()>>,
    scopes: Vec<scopes::ScopeFrame>,
    work: Arc<InvocationWork>,
    control: Arc<crate::WorkbenchExecutionControl>,
    publication: CheckpointPublication,
}

impl ToolCursor {
    pub(super) fn new(
        work: Arc<InvocationWork>,
        control: Arc<crate::WorkbenchExecutionControl>,
        publication: CheckpointPublication,
    ) -> Self {
        Self {
            green: None,
            scopes: Vec::new(),
            work,
            control,
            publication,
        }
    }

    pub(super) fn realm(&self, actor_realm: RealmId) -> RealmId {
        self.scopes
            .last()
            .map(|frame| frame.realm)
            .or_else(|| self.green.as_ref().and_then(GreenInvocation::active_realm))
            .unwrap_or(actor_realm)
    }

    pub(super) fn is_main_terminal(&self) -> bool {
        self.scopes.is_empty()
            && self
                .green
                .as_ref()
                .and_then(GreenInvocation::active_work)
                .is_none()
    }

    pub(super) fn check_admission(
        &mut self,
        kernel: &KernelContext,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let error = match kernel.requested_shutdown() {
            Some(terminal) => Some(ResidentActorWorkbenchError::RetiredBeforeAdmission(
                terminal,
            )),
            None if self.control.cancellation_requested() => Some(
                ResidentActorWorkbenchError::ActorProtocol("tool invocation cancelled".into()),
            ),
            None => None,
        };
        if let Some(error) = error {
            self.work.close();
            if let Some(green) = &mut self.green {
                green.cancel_parent();
            }
            return Err(error);
        }
        Ok(())
    }

    fn owner(&self) -> CurrentEffectOwner<'static> {
        let base = CurrentEffectOwner::Tool {
            work: self.work.clone(),
            publication: self.publication.clone(),
            control: self.control.clone(),
        };
        let scope = self
            .scopes
            .last()
            .map(|frame| frame.work.clone())
            .or_else(|| self.green.as_ref().and_then(GreenInvocation::active_work));
        match scope {
            Some(scope) => CurrentEffectOwner::Scoped {
                base: Box::new(base),
                scope,
                wait_control: self
                    .green
                    .as_ref()
                    .and_then(GreenInvocation::active_wait_control),
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
                let green = cursor.green.get_or_insert_with(Default::default);
                match self.prepare_green_operation(
                    kernel,
                    context,
                    &owner,
                    green,
                    &mut cursor.scopes,
                    boundary,
                )? {
                    GreenAdvance::Start {
                        callback,
                        realm,
                        token,
                        work,
                    } => {
                        return self
                            .environment
                            .runner
                            .run_owned_scope_callback(context.clone(), callback, realm, token, work)
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
                        return self
                            .environment
                            .runner
                            .resume_value(
                                context.clone(),
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
                return self
                    .environment
                    .runner
                    .run_owned_scope_callback(context.clone(), callback, realm, token, work)
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
                let operation = scopes::finish_scope(
                    self.environment.clone(),
                    kernel.clone(),
                    context.clone(),
                    frame,
                    body,
                );
                match &mut cursor.green {
                    Some(green) => green.enqueue_frontier(
                        std::mem::take(&mut cursor.scopes),
                        Box::pin(async move { (operation.await, ()) }),
                    ),
                    None => return operation.await,
                }
            }
            boundary if cursor.green.is_none() => {
                return self
                    .resolve_effect(
                        kernel,
                        context,
                        owner,
                        &crate::CallAncestry::begin(context.actor),
                        boundary,
                    )
                    .await;
            }
            boundary => {
                let boundary = prepare_execution_effect(context, &owner, boundary);
                let (boundary, _) = self
                    .prepare_display_boundary(
                        context,
                        boundary,
                        DEFAULT_DISPLAY_CHARACTER_ALLOWANCE,
                        None,
                        None,
                    )
                    .await?;
                match self.prepare_invocation_wait(kernel, context, &owner, boundary)? {
                    Ok(wait) => {
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
                        cursor
                            .green
                            .as_mut()
                            .expect("tool async cursor")
                            .enqueue_frontier(
                                std::mem::take(&mut cursor.scopes),
                                Box::pin(async move {
                                    let result = operation.await;
                                    // Named notebook result bindings are never prepared by this adapter.
                                    (result.outcome, ())
                                }),
                            );
                    }
                    Err(boundary) => {
                        // Actor protocol transitions stay under the original exclusive tool owner.
                        return self
                            .resolve_effect(
                                kernel,
                                context,
                                owner,
                                &crate::CallAncestry::begin(context.actor),
                                boundary,
                            )
                            .await;
                    }
                }
            }
        }
        self.next_tool_frontier(kernel, context, cursor).await
    }

    async fn next_tool_frontier(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        cursor: &mut ToolCursor,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        loop {
            cursor.check_admission(kernel)?;
            let retirement = kernel.retained_exit();
            let green = cursor.green.as_mut().expect("tool async cursor");
            let completion = tokio::select! {
                biased;
                () = cursor.control.wait_for_cancellation() => {
                    cursor.work.close(); green.cancel_parent();
                    return Err(ResidentActorWorkbenchError::ActorProtocol("tool invocation cancelled".into()));
                }
                terminal = retirement.wait_requested_shutdown() => {
                    cursor.work.close(); green.cancel_parent();
                    return Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(terminal));
                }
                completion = green.next() => completion?,
            };
            if let Some(mut frontier) = self.apply_green_frontier(context, green, completion)? {
                cursor.scopes = frontier.scopes;
                return frontier.result.take().expect("ready native tool frontier");
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
}
