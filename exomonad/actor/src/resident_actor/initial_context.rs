//! Internal startup invocation and its retained actor lifecycle.

use super::*;
use crate::initial_context::{InitialContextEvent, INITIAL_CONTEXT_SLOT};
use crate::{
    ApplicationConversation, InitialContextIdentity, InitialContextObservation,
    InitialContextPublication, InitialContextState, RetainedContextSeed,
};

fn failure(error: impl std::fmt::Display) -> ResidentActorWorkbenchError {
    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
}

struct InitialContextWaitGuard(Option<Arc<crate::WorkbenchExecutionControl>>);

impl Drop for InitialContextWaitGuard {
    fn drop(&mut self) {
        if let Some(control) = self.0.take() {
            control.request_cancellation();
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Retention belongs to execution settlement, before its reply is released.
    /// Losing a host waiter cannot discard a completed startup result.
    pub(super) fn retain_initial_context_execution(
        &self,
        actor: ActorRef,
        result: &Result<KernelStep<WorkbenchResponse>, crate::KernelInvocationFailure>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let change = match result {
            Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
            | Ok(KernelStep::Stop {
                output: response, ..
            }) => {
                let receipts = serde_json::to_value(&response.items).map_err(failure)?;
                if response.status == WorkbenchRunStatus::Committed {
                    match response
                        .items
                        .first()
                        .and_then(|item| item.value.clone())
                        .map(serde_json::from_value::<RetainedContextSeed>)
                        .transpose()
                    {
                        Ok(Some(seed)) => InitialContextEvent::Retain {
                            result: seed,
                            receipts,
                        },
                        invalid => InitialContextEvent::Uncertain {
                            detail: format!(
                                "initial context completed without a decodable semantic seed: {invalid:?}"
                            ),
                            receipts,
                        },
                    }
                } else {
                    InitialContextEvent::Fail {
                        detail: "initial context execution did not commit".into(),
                        cancelled: response.status == WorkbenchRunStatus::RequestCancelled,
                        receipts,
                    }
                }
            }
            Err(error) => {
                let receipts = serde_json::to_value(error.receipts()).map_err(failure)?;
                if matches!(
                    error,
                    crate::KernelInvocationFailure::CleanupUnconfirmed { .. }
                ) || matches!(
                    error.publication(),
                    Some(WorkbenchPublicationOutcome::RecoveryRequired { .. })
                ) {
                    InitialContextEvent::Uncertain {
                        detail: error.to_string(),
                        receipts,
                    }
                } else {
                    InitialContextEvent::Fail {
                        detail: error.to_string(),
                        cancelled: matches!(
                            error,
                            crate::KernelInvocationFailure::Cancelled { .. }
                        ),
                        receipts,
                    }
                }
            }
        };
        let owner = self
            .environment
            .actors
            .lock()
            .get(&actor)
            .cloned()
            .ok_or_else(|| failure("initial context execution has no actor owner"))?;
        let mut current = owner.initial_context.lock();
        let next = if let Some(journal) = &self.environment.recovery {
            journal
                .record_initial_context(actor, change)
                .map_err(failure)?
        } else {
            let mut next = current.clone();
            crate::initial_context::apply_event(&mut next, change).map_err(failure)?;
            next.expect("retained execution has admitted initialization")
        };
        *current = Some(next);
        Ok(())
    }
}

impl<H, O> ResidentForest<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    fn initial_context_record(
        &self,
        actor: ActorRef,
    ) -> Result<ResidentActorRecord, ResidentActorWorkbenchError> {
        self.environment
            .actors
            .lock()
            .get(&actor)
            .cloned()
            .filter(|record| record.terminal.is_none())
            .ok_or_else(|| failure("initial context actor is unavailable"))
    }

    pub fn initial_context_observation(
        &self,
        actor: ActorRef,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let record = self.initial_context_record(actor)?;
        let current = record.initial_context.lock();
        if let Some(current) = current.as_ref() {
            return Ok(current.clone());
        }
        self.environment
            .recovery
            .as_ref()
            .map(|journal| journal.initial_context(actor).map_err(failure))
            .transpose()?
            .flatten()
            .ok_or_else(|| failure("initial context has not been admitted"))
    }

    /// Project retained facts into the ordinary synchronous context effect.
    /// This observation never executes the initializer or modifies context.
    pub fn initial_context_wire_observation(
        &self,
        actor: ActorRef,
    ) -> Result<tidepool_bridge_effects::InitialContextObservation, ResidentActorWorkbenchError>
    {
        use tidepool_bridge_effects::{
            ContextReference, InitialContextIdentity as Identity,
            InitialContextObservation as Wire, InitialContextPublicationReceipt as Receipt,
        };
        let observed = self.initial_context_observation(actor)?;
        match observed.state {
            InitialContextState::AbsentHook => return Ok(Wire::InitialContextAbsent),
            InitialContextState::Captured => return Ok(Wire::InitialContextCaptured),
            _ => {}
        }
        let original = observed
            .identity
            .ok_or_else(|| failure("startup observation has no issued identity"))?;
        let identity = Identity {
            operation: original.operation.as_str().to_owned(),
            actor: (
                i64::try_from(original.actor.id.0).map_err(failure)?,
                i64::try_from(original.actor.incarnation.0).map_err(failure)?,
            ),
            spec_install: i64::try_from(original.spec_install).map_err(failure)?,
            spec_revision: original.spec_revision,
            spec_entry: original.spec_entry,
            spec_source: original.spec_source,
        };
        Ok(match observed.state {
            InitialContextState::Preparing => Wire::InitialContextPreparing(identity),
            InitialContextState::ResultRetained => Wire::InitialContextRetained(
                identity,
                observed
                    .result
                    .ok_or_else(|| failure("startup retained result is absent"))?
                    .rendered,
            ),
            InitialContextState::Published => {
                let publication = observed
                    .publication
                    .ok_or_else(|| failure("startup publication evidence is absent"))?;
                let ApplicationConversation::Embedded {
                    run,
                    agent_path,
                    incarnation,
                } = publication.conversation
                else {
                    return Err(failure(
                        "startup publication has unsupported conversation transport",
                    ));
                };
                let receipt = Receipt {
                    run,
                    actor_path: agent_path,
                    conversation_incarnation: incarnation,
                    request: publication.request,
                    caller_reference: publication.caller_reference.map(ContextReference::new),
                };
                let result = observed
                    .result
                    .ok_or_else(|| failure("startup published result is absent"))?;
                match publication.authored_reference {
                    Some(reference) => Wire::InitialContextPublished(
                        identity,
                        receipt,
                        result.rendered,
                        ContextReference::new(reference),
                    ),
                    None => Wire::InitialContextPublishedEmpty(identity, receipt),
                }
            }
            InitialContextState::Failed { detail } => Wire::InitialContextFailed(identity, detail),
            InitialContextState::Cancelled => Wire::InitialContextCancelled(identity),
            InitialContextState::RecoveryRequired { detail } => {
                Wire::InitialContextUncertain(identity, detail)
            }
            InitialContextState::AbsentHook | InitialContextState::Captured => {
                unreachable!("handled without identity")
            }
        })
    }

    fn change_initial_context(
        &self,
        actor: ActorRef,
        change: InitialContextEvent,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let record = self.initial_context_record(actor)?;
        let mut current = record.initial_context.lock();
        let owner = current
            .as_ref()
            .and_then(|state| state.identity.as_ref())
            .map_or(actor, |identity| identity.actor);
        let next = if let Some(journal) = &self.environment.recovery {
            journal
                .record_initial_context(owner, change)
                .map_err(failure)?
        } else {
            let mut next = current.clone();
            crate::initial_context::apply_event(&mut next, change).map_err(failure)?;
            next.ok_or_else(|| failure("initial context transition produced no result"))?
        };
        *current = Some(next.clone());
        Ok(next)
    }

    /// Admit one startup operation using the actor's exact installed policy.
    /// The mailbox runs it as an owned parked workbench execution, leaving
    /// native scheduling and host service pumps available while it waits.
    pub async fn prepare_initial_context(
        &self,
        actor: ActorRef,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let record = self.initial_context_record(actor)?;
        let invocation = {
            let mut current = record.initial_context.lock();
            if let Some(current) = current.as_ref() {
                return Ok(current.clone());
            }
            if let Some(restored) = self
                .environment
                .recovery
                .as_ref()
                .map(|journal| journal.initial_context(actor).map_err(failure))
                .transpose()?
                .flatten()
            {
                *current = Some(restored.clone());
                return Ok(restored);
            }
            if matches!(
                record.installed_tools.observe(),
                crate::resident_workbench::InstalledToolsObservation::SourceUnavailable(_)
            ) {
                return Err(failure("initial context installed source is unavailable"));
            }
            let lease = record.installed_tools.current();
            let tools = lease.as_ref().and_then(crate::InstalledToolLease::tools);
            let captured = record.descriptor.context_parent().is_some();
            let change = if captured
                || tools.is_none_or(|tools| {
                    !tools.slots.iter().any(|slot| slot == INITIAL_CONTEXT_SLOT)
                }) {
                InitialContextEvent::Skip {
                    captured,
                    caller_seed: record.fresh_context_seed.clone(),
                }
            } else {
                let tools = tools.expect("installed startup slot");
                let (_, operation) =
                    ensure_workbench_execution_id(WorkbenchRequest::for_retained_operation(
                        INITIAL_CONTEXT_SLOT,
                        serde_json::Value::Null,
                    ));
                let source = lease
                    .as_ref()
                    .expect("installed startup source")
                    .source()
                    .semantic_digest();
                InitialContextEvent::Begin {
                    identity: InitialContextIdentity {
                        operation,
                        actor,
                        spec_install: tools.install,
                        spec_revision: tools.revision.clone(),
                        spec_entry: tools
                            .origin
                            .resolved()
                            .and_then(|resolved| resolved.entry.clone()),
                        spec_source: source.iter().map(|byte| format!("{byte:02x}")).collect(),
                    },
                    caller_seed: record.fresh_context_seed.clone(),
                }
            };
            let next = if let Some(journal) = &self.environment.recovery {
                journal
                    .record_initial_context(actor, change)
                    .map_err(failure)?
            } else {
                let mut next = None;
                crate::initial_context::apply_event(&mut next, change).map_err(failure)?;
                next.expect("initial admission produces observation")
            };
            *current = Some(next.clone());
            let Some(identity) = next.identity.clone() else {
                return Ok(next);
            };
            let request = WorkbenchRequest::for_retained_operation(INITIAL_CONTEXT_SLOT,
                serde_json::json!({"operation": identity.operation.as_str(), "callerSeed": next.caller_seed}))
                .with_execution_id(identity.operation);
            let mut invocation = crate::ActorWorkbenchInvocation::issued(request, lease, None);
            invocation.initial_context = true;
            invocation
        };
        let local = self
            .directory
            .resolve(actor)
            .ok_or_else(|| failure("initial context actor disappeared before admission"))?;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let control = crate::WorkbenchExecutionControl::untracked();
        let mut waiter = InitialContextWaitGuard(Some(control.clone()));
        local
            .admit_mailbox(crate::KernelMessage::Workbench {
                invocation,
                control: Some(control),
                reply: reply.into(),
            })
            .map_err(failure)?;
        receiver
            .await
            .map_err(|_| failure("initial context execution ended without a retained outcome"))?
            .map_err(failure)?;
        waiter.0.take();
        self.initial_context_observation(actor)
    }

    /// Acknowledge publication into the current exact conversation. References
    /// remain historical evidence; the context owner alone authorizes edits.
    pub fn acknowledge_initial_context_publication(
        &self,
        actor: ActorRef,
        operation: &WorkbenchExecutionId,
        publication: InitialContextPublication,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let observed = self.initial_context_observation(actor)?;
        if observed
            .identity
            .as_ref()
            .is_none_or(|identity| &identity.operation != operation)
        {
            return Err(failure(
                "initial context publication has another operation identity",
            ));
        }
        if let Some(journal) = &self.environment.recovery {
            if !journal
                .initial_context_publication_matches(actor, operation, &publication.conversation)
                .map_err(failure)?
            {
                return Err(failure(
                    "initial context publication has another conversation association",
                ));
            }
        }
        let record = self.initial_context_record(actor)?;
        if record.initial_context.lock().is_none() {
            *record.initial_context.lock() = Some(observed);
        }
        self.change_initial_context(actor, InitialContextEvent::Publish { publication })
    }
}
