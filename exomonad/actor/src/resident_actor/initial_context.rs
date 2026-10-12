//! Internal startup invocation and its retained actor lifecycle.

use super::*;
use crate::initial_context::{InitialContextEvent, INITIAL_CONTEXT_SLOT};
use crate::{InitialContextIdentity, InitialContextObservation, InitialContextPublication,
    InitialContextState, RetainedContextSeed};

fn failure(error: impl std::fmt::Display) -> ResidentActorWorkbenchError {
    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
}

impl<H, O> ResidentForest<H, O>
where H: DispatchEffect<O> + Send + 'static, O: OutputSink + Sync + 'static {
    fn initial_context_record(&self, actor: ActorRef) -> Result<ResidentActorRecord, ResidentActorWorkbenchError> {
        self.environment.actors.lock().get(&actor).cloned()
            .filter(|record| record.terminal.is_none())
            .ok_or_else(|| failure("initial context actor is unavailable"))
    }

    pub fn initial_context_observation(&self, actor: ActorRef) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let record = self.initial_context_record(actor)?;
        let current = record.initial_context.lock();
        if let Some(current) = current.as_ref() { return Ok(current.clone()); }
        self.environment.recovery.as_ref()
            .map(|journal| journal.initial_context(actor).map_err(failure)).transpose()?
            .flatten().ok_or_else(|| failure("initial context has not been admitted"))
    }

    fn change_initial_context(&self, actor: ActorRef, change: InitialContextEvent) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let record = self.initial_context_record(actor)?;
        let mut current = record.initial_context.lock();
        let owner = current.as_ref().and_then(|state| state.identity.as_ref()).map_or(actor, |identity| identity.actor);
        let next = if let Some(journal) = &self.environment.recovery {
            journal.record_initial_context(owner, change).map_err(failure)?
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
    pub async fn prepare_initial_context(&self, actor: ActorRef) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let record = self.initial_context_record(actor)?;
        let invocation = {
            let mut current = record.initial_context.lock();
            if let Some(current) = current.as_ref() { return Ok(current.clone()); }
            if let Some(restored) = self.environment.recovery.as_ref()
                .map(|journal| journal.initial_context(actor).map_err(failure)).transpose()?.flatten() {
                *current = Some(restored.clone());
                return Ok(restored);
            }
            let lease = record.installed_tools.current().ok_or_else(|| failure("initial context has no installed source policy"))?;
            let tools = lease.tools().ok_or_else(|| failure("initial context has no installed dispatcher"))?;
            let captured = record.descriptor.context_parent().is_some();
            let change = if captured || !tools.slots.iter().any(|slot| slot == INITIAL_CONTEXT_SLOT) {
                InitialContextEvent::Skip { captured, caller_seed: record.fresh_context_seed.clone() }
            } else {
                let mut digest = blake3::Hasher::new();
                digest.update(b"exomonad-initial-context-v1");
                digest.update(&actor.id.0.to_le_bytes());
                digest.update(&actor.incarnation.0.to_le_bytes());
                digest.update(&tools.install.to_le_bytes());
                let source = lease.source().semantic_digest();
                digest.update(&source);
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&digest.finalize().as_bytes()[..16]);
                InitialContextEvent::Begin {
                    identity: InitialContextIdentity {
                        operation: WorkbenchExecutionId::from_digest(bytes), actor,
                        spec_install: tools.install, spec_revision: tools.revision.clone(),
                        spec_entry: tools.origin.resolved().and_then(|resolved| resolved.entry.clone()),
                        spec_source: source.iter().map(|byte| format!("{byte:02x}")).collect(),
                    },
                    caller_seed: record.fresh_context_seed.clone(),
                }
            };
            let next = if let Some(journal) = &self.environment.recovery {
                journal.record_initial_context(actor, change).map_err(failure)?
            } else {
                let mut next = None;
                crate::initial_context::apply_event(&mut next, change).map_err(failure)?;
                next.expect("initial admission produces observation")
            };
            *current = Some(next.clone());
            let Some(identity) = next.identity.clone() else { return Ok(next); };
            let request = WorkbenchRequest::for_retained_operation(INITIAL_CONTEXT_SLOT,
                serde_json::json!({"operation": identity.operation.as_str(), "callerSeed": next.caller_seed}))
                .with_execution_id(identity.operation);
            let mut invocation = crate::ActorWorkbenchInvocation::issued(request, Some(lease), None);
            invocation.initial_context = true;
            invocation
        };
        let local = self.directory.resolve(actor).ok_or_else(|| failure("initial context actor disappeared before admission"))?;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        local.admit_mailbox(crate::KernelMessage::Workbench {
            invocation, control: Some(crate::WorkbenchExecutionControl::untracked()), reply: reply.into(),
        }).map_err(failure)?;
        let response = receiver.await.map_err(|_| failure("initial context execution ended without a retained outcome"))?;
        let change = match response {
            Ok(response) => {
                let receipts = serde_json::to_value(&response.items).map_err(failure)?;
                if response.status == WorkbenchRunStatus::Committed {
                    let seed = response.items.first().and_then(|item| item.value.clone())
                        .ok_or_else(|| failure("initial context completed without a semantic seed"))?;
                    InitialContextEvent::Retain { result: serde_json::from_value::<RetainedContextSeed>(seed).map_err(failure)?, receipts }
                } else {
                    InitialContextEvent::Fail { detail: "initial context execution did not commit".into(), cancelled: false, receipts }
                }
            }
            Err(error) => InitialContextEvent::Fail { detail: error.to_string(), cancelled: matches!(error, crate::KernelInvocationFailure::Cancelled { .. }),
                receipts: serde_json::to_value(error.receipts()).map_err(failure)? },
        };
        self.change_initial_context(actor, change)
    }

    /// Acknowledge publication into the current exact conversation. References
    /// remain historical evidence; the context owner alone authorizes edits.
    pub fn acknowledge_initial_context_publication(&self, actor: ActorRef,
        operation: &WorkbenchExecutionId, publication: InitialContextPublication,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let observed = self.initial_context_observation(actor)?;
        if observed.identity.as_ref().is_none_or(|identity| &identity.operation != operation) {
            return Err(failure("initial context publication has another operation identity"));
        }
        if let Some(journal) = &self.environment.recovery {
            if journal.active_application_conversation(actor).as_ref() != Some(&publication.conversation) {
                return Err(failure("initial context publication has another conversation association"));
            }
        }
        let record = self.initial_context_record(actor)?;
        if record.initial_context.lock().is_none() {
            *record.initial_context.lock() = Some(observed);
        }
        self.change_initial_context(actor, InitialContextEvent::Publish { publication })
    }
}
