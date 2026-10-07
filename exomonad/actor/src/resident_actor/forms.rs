//! Interpret generated form operations through exact native frame ownership.
use super::*;
use crate::forms::{attempt_id, lease_id, FormCleanup, MountedForm};
use std::time::Duration;
use tidepool_bridge_effects::{FormAttempt, FormAttemptId, FormCause, FormLease, FormTransition};

pub(crate) enum FormOperation {
    Open(serde_json::Value),
    Await(FormLease),
    Reject(FormLease, FormAttemptId, serde_json::Value),
    Commit(FormLease, FormAttemptId, serde_json::Value),
    Close(FormLease),
}
impl FormOperation {
    pub(crate) fn decode(
        request: crate::generated::ask_user::AskUserReq,
        table: &tidepool_repr::DataConTable,
    ) -> Self {
        use crate::generated::ask_user::AskUserReq::*;
        match request {
            FormOpenWith(descriptor) => {
                Self::Open(tidepool_runtime::value_to_json(&descriptor, table, 0))
            }
            FormAwaitWith(lease) => Self::Await(lease),
            FormRejectWith(lease, attempt, errors) => Self::Reject(
                lease,
                attempt,
                tidepool_runtime::value_to_json(&errors, table, 0),
            ),
            FormCommitWith(lease, attempt, view) => Self::Commit(
                lease,
                attempt,
                tidepool_runtime::value_to_json(&view, table, 0),
            ),
            FormCloseWith(lease) => Self::Close(lease),
        }
    }
}

fn resolve<H, O>(
    environment: &ResidentEnvironment<H, O>,
    context: &ActorSessionContext,
    realm: RealmId,
    lease: &FormLease,
) -> Result<Arc<MountedForm>, FormCause> {
    let mounted = environment
        .form_registry
        .lock()
        .get(lease_id(lease))
        .and_then(std::sync::Weak::upgrade)
        .ok_or(FormCause::FormClosed)?;
    if mounted.cleanup.actor != context.actor
        || mounted.session != context.placement.session
        || mounted.realm != realm
    {
        return Err(FormCause::FormUnauthorized);
    }
    Ok(mounted)
}
fn admitted<T>(
    control: Option<&Arc<crate::WorkbenchExecutionControl>>,
    work: &InvocationWork,
    operation: impl FnOnce() -> Result<T, FormCause>,
) -> Result<T, FormCause> {
    let admission = || {
        work.with_admission(operation)
            .map_err(|_| FormCause::FormClosed)?
    };
    match control {
        Some(control) => control
            .admit_interaction(admission)
            .ok_or(FormCause::FormClosed)?,
        None => admission(),
    }
}

pub(super) fn service<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    work: Option<Arc<InvocationWork>>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    interaction_control: Option<Arc<crate::WorkbenchExecutionControl>>,
    continuation: ResidentHole,
    operation: FormOperation,
    publication: crate::FormPublication,
) -> futures_util::future::BoxFuture<'static, Result<ResidentOutcome, ResidentActorWorkbenchError>>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    Box::pin(async move {
        let realm = environment
            .runner
            .form_continuation_realm(context.clone(), continuation.clone())
            .await?;
        match operation {
            FormOperation::Open(descriptor) => {
                let result: Result<FormLease, FormCause> = async {
                    let host = environment
                        .form_host
                        .clone()
                        .ok_or(FormCause::FormNotInstalled)?;
                    let work = work.as_ref().ok_or(FormCause::FormUnauthorized)?;
                    let mount = uuid::Uuid::new_v4().to_string();
                    let cleanup = FormCleanup::new(context.actor, mount.clone(), host.clone());
                    let open = || {
                        work.admit_form(cleanup.clone(), || {
                            host.open(&publication, &mount, &descriptor)
                        })
                    };
                    match interaction_control.as_ref() {
                        Some(control) => control
                            .admit_interaction(open)
                            .ok_or(FormCause::FormClosed)??,
                        None => open()?,
                    }
                    let mounted = Arc::new(MountedForm {
                        cleanup: cleanup.clone(),
                        session: context.placement.session,
                        realm,
                    });
                    // The registry observes leases but never extends their lifetime.
                    {
                        let mut registry = environment.form_registry.lock();
                        registry.retain(|_, lease| lease.strong_count() > 0);
                        registry.insert(mount.clone(), Arc::downgrade(&mounted));
                    }
                    environment
                        .runner
                        .retain_form_lease(context.clone(), continuation.clone(), mounted)
                        .await
                        .map_err(|error| FormCause::FormTransportFailed(error.to_string()))?;
                    if cleanup.is_closed() {
                        return Err(FormCause::FormClosed);
                    }
                    Ok(FormLease::FormLeaseToken(mount))
                }
                .await;
                environment
                    .runner
                    .resume_value(context, continuation, result)
                    .await
            }
            FormOperation::Await(lease) => {
                let control = control.unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
                control.arm_sleep();
                let retirement = kernel.retained_exit();
                let result: Result<FormAttempt, FormCause> = async {
                    let mounted = resolve(&environment, &context, realm, &lease)?;
                    let work = work.as_ref().ok_or(FormCause::FormUnauthorized)?;
                    loop {
                        if mounted.cleanup.is_closed() { return Err(FormCause::FormClosed); }
                        if let Some(attempt) = admitted(interaction_control.as_ref(), work, || mounted.cleanup.host.attempt(context.actor, &mounted.cleanup.mount))? { return Ok(attempt); }
                        tokio::select! {
                            biased;
                            _ = retirement.wait_requested_shutdown() => { control.request_cancellation(); return Err(FormCause::FormClosed); }
                            () = control.wait_for_cancellation() => return Err(FormCause::FormClosed),
                            () = tokio::time::sleep(Duration::from_millis(25)) => {}
                        }
                    }
                }.await;
                if !control.claim_expiry()
                    && control
                        .native_cancel()
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    let (outcome, consumed) = environment
                        .runner
                        .abort_live(
                            context,
                            continuation,
                            "human form interrupted by owner cancellation".into(),
                        )
                        .await;
                    if consumed {
                        control.acknowledge_cancellation();
                    }
                    return outcome;
                }
                let outcome = environment
                    .runner
                    .resume_value(context, continuation, result)
                    .await;
                control.finish_sleep();
                outcome
            }
            FormOperation::Reject(lease, attempt, errors) => {
                let result: Result<FormTransition, FormCause> = (|| {
                    let mounted = resolve(&environment, &context, realm, &lease)?;
                    if mounted.cleanup.is_closed() {
                        return Err(FormCause::FormClosed);
                    }
                    let work = work.as_ref().ok_or(FormCause::FormUnauthorized)?;
                    admitted(interaction_control.as_ref(), work, || {
                        mounted.cleanup.host.reject(
                            context.actor,
                            &mounted.cleanup.mount,
                            attempt_id(&attempt),
                            &errors,
                        )
                    })
                })();
                environment
                    .runner
                    .resume_value(context, continuation, result)
                    .await
            }
            FormOperation::Commit(lease, attempt, presentation) => {
                let result: Result<FormTransition, FormCause> = (|| {
                    let mounted = resolve(&environment, &context, realm, &lease)?;
                    if mounted.cleanup.is_closed() {
                        return Err(FormCause::FormClosed);
                    }
                    let work = work.as_ref().ok_or(FormCause::FormUnauthorized)?;
                    admitted(interaction_control.as_ref(), work, || {
                        let result = mounted.cleanup.host.commit(
                            context.actor,
                            &mounted.cleanup.mount,
                            attempt_id(&attempt),
                            &presentation,
                        )?;
                        if result == FormTransition::FormApplied {
                            mounted.cleanup.settled();
                        }
                        Ok(result)
                    })
                })();
                environment
                    .runner
                    .resume_value(context, continuation, result)
                    .await
            }
            FormOperation::Close(lease) => {
                let result: Result<(), FormCause> = resolve(&environment, &context, realm, &lease)
                    .and_then(|mounted| mounted.cleanup.close());
                environment
                    .runner
                    .resume_value(context, continuation, result)
                    .await
            }
        }
    })
}
