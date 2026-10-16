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

fn initial_context_execution_event(
    result: &Result<KernelStep<WorkbenchResponse>, crate::KernelInvocationFailure>,
) -> Result<InitialContextEvent, ResidentActorWorkbenchError> {
    Ok(match result {
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
                    cancelled: error.is_cancelled(),
                    receipts,
                }
            }
        }
    })
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
    pub(super) async fn retain_initial_context_execution(
        lifecycle: &crate::LifecycleHandle,
        _actor: ActorRef,
        operation: &WorkbenchExecutionId,
        result: &Result<KernelStep<WorkbenchResponse>, crate::KernelInvocationFailure>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let change = initial_context_execution_event(result)?;
        lifecycle
            .change_initial_context(change, operation.clone())
            .await
            .map_err(failure)?;
        Ok(())
    }
}

impl<H, O> ResidentForest<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async fn initial_context_record(
        &self,
        actor: ActorRef,
    ) -> Result<
        (
            crate::LifecycleHandle,
            crate::kernel::lifecycle::resident::ResidentSnapshot,
        ),
        ResidentActorWorkbenchError,
    > {
        let lifecycle = self.lifecycle_for(actor).await?;
        let snapshot = lifecycle
            .resident_snapshot()
            .ok_or_else(|| failure("initial context owner is unconfigured"))?;
        Ok((lifecycle, snapshot))
    }

    pub async fn initial_context_observation(
        &self,
        actor: ActorRef,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        self.initial_context_record(actor)
            .await?
            .1
            .initial_context
            .ok_or_else(|| failure("initial context has not been admitted"))
    }

    /// Project retained facts into the ordinary synchronous context effect.
    /// This observation never executes the initializer or modifies context.
    pub async fn initial_context_wire_observation(
        &self,
        actor: ActorRef,
    ) -> Result<tidepool_bridge_effects::InitialContextObservation, ResidentActorWorkbenchError>
    {
        wire_initial_context(self.initial_context_observation(actor).await?)
    }

    /// Observe under one exact provider-owner admission and native transition.
    pub async fn initial_context_wire_observation_for_provider(
        &self,
        admission: &ActorProviderAdmission,
    ) -> Result<tidepool_bridge_effects::InitialContextObservation, ResidentActorWorkbenchError>
    {
        if !self.directory.same_owner(&admission.issuer) {
            return Err(failure(
                "provider attachment belongs to another forest owner",
            ));
        }
        let observed = admission
            .lifecycle
            .provider_initial_observation(admission.owner.clone())
            .await
            .map_err(failure)?;
        wire_initial_context(observed)
    }

    /// Retain publication and native settlement before releasing the provider reply.
    pub async fn acknowledge_initial_context_publication_for_provider(
        &self,
        admission: &ActorProviderAdmission,
        operation: &WorkbenchExecutionId,
        publication: InitialContextPublication,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        if !self.directory.same_owner(&admission.issuer) {
            return Err(failure(
                "provider attachment belongs to another forest owner",
            ));
        }
        admission
            .lifecycle
            .provider_initial_acknowledge(admission.owner.clone(), operation.clone(), publication)
            .await
            .map_err(failure)
    }

    /// Admit one startup operation using the actor's exact installed policy.
    /// The mailbox runs it as an owned parked workbench execution, leaving
    /// native scheduling and host service pumps available while it waits.
    pub async fn prepare_initial_context(
        &self,
        actor: ActorRef,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        let (lifecycle, record) = self.initial_context_record(actor).await?;
        if let Some(current) = record.initial_context {
            return Ok(current);
        }
        let lease = record.installed_policy;
        let tools = lease.as_ref().and_then(crate::InstalledToolLease::tools);
        let captured = record.descriptor.context_parent().is_some();
        let change = if captured || tools.is_none_or(|tools| tools.initial_context.is_none()) {
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
                    spec_revision: tools.revision().map(str::to_owned),
                    spec_entry: tools
                        .resolved_spec()
                        .and_then(|resolved| resolved.entry.clone()),
                    spec_source: source.iter().map(|byte| format!("{byte:02x}")).collect(),
                },
                caller_seed: record.fresh_context_seed.clone(),
            }
        };
        let transition = lifecycle
            .begin_initial_context(change)
            .await
            .map_err(failure)?;
        let Some(execution) = transition.execution else {
            return Ok(transition.observation);
        };
        let mut waiter = InitialContextWaitGuard(Some(execution.control));
        execution
            .answer
            .await
            .map_err(|_| failure("initial context execution ended without retained outcome"))?
            .map_err(failure)?;
        waiter.0.take();
        self.initial_context_observation(actor).await
    }

    /// Acknowledge publication into the current exact conversation. References
    /// remain historical evidence; the context owner alone authorizes edits.
    pub async fn acknowledge_initial_context_publication(
        &self,
        actor: ActorRef,
        operation: &WorkbenchExecutionId,
        publication: InitialContextPublication,
    ) -> Result<InitialContextObservation, ResidentActorWorkbenchError> {
        self.lifecycle_for(actor)
            .await?
            .acknowledge_initial_context(operation.clone(), publication)
            .await
            .map_err(failure)
    }
}

fn wire_initial_context(
    observed: InitialContextObservation,
) -> Result<tidepool_bridge_effects::InitialContextObservation, ResidentActorWorkbenchError> {
    use tidepool_bridge_effects::{
        ContextReference, InitialContextIdentity as Identity, InitialContextObservation as Wire,
        InitialContextPublicationReceipt as Receipt,
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{PreparedFailureKind, PreparedRuntimeError};

    fn receipt(operation: &WorkbenchExecutionId) -> WorkbenchItemReceipt {
        WorkbenchItemReceipt {
            presentation: None,
            index: 0,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::Rejected,
            output: String::new(),
            value: None,
            diagnostics: Vec::new(),
            failure_layer: None,
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations: [
                ("FsRead", WorkbenchOperationDisposition::Committed),
                ("FsWrite", WorkbenchOperationDisposition::Committed),
                ("Jev", WorkbenchOperationDisposition::Unknown),
            ]
            .into_iter()
            .enumerate()
            .map(
                |(effect_ordinal, (effect, disposition))| WorkbenchOperationReceipt {
                    display: None,
                    display_publication: None,
                    id: WorkbenchOperationId {
                        execution: operation.clone(),
                        input_unit_index: 0,
                        effect_ordinal,
                    },
                    effect: effect.into(),
                    disposition,
                },
            )
            .collect(),
            terminal_transfer: None,
        }
    }

    #[tokio::test]
    async fn typed_execution_cancellation_retains_journal_state_and_effect_receipts() {
        for case in 0..4 {
            let root = Arc::new(tempfile::tempdir().unwrap());
            let anchor =
                tidepool_atomic_write::DirectoryAnchor::open_existing(root.path()).unwrap();
            let journal = crate::ActorRecoveryJournal::open(&anchor, "actors.jsonl", root.clone())
                .await
                .unwrap();
            let actor = ActorRef::first(crate::ActorId(7));
            let descriptor = ActorDescriptor::new(
                "journal retention",
                crate::ActorPlacement {
                    session: tidepool_repr::SessionId(1),
                    lexical_scope: ScopeId::ROOT,
                    resource_scope: RealmId::ROOT,
                },
            );
            journal.admit(actor, &descriptor, &[]).await.unwrap();
            let operation = WorkbenchExecutionId::from_digest([case; 16]);
            journal
                .record_initial_context(
                    actor,
                    InitialContextEvent::Begin {
                        identity: InitialContextIdentity {
                            operation: operation.clone(),
                            actor,
                            spec_install: 1,
                            spec_revision: None,
                            spec_entry: None,
                            spec_source: "journal-retention-data".into(),
                        },
                        caller_seed: None,
                    },
                )
                .await
                .unwrap();
            let item = receipt(&operation);
            let expected_receipts = serde_json::to_value([item.clone()]).unwrap();
            let source = if case == 1 {
                // Identical text cannot turn an ordinary actor failure into cancellation.
                ResidentActorWorkbenchError::ActorProtocol("prepared execution cancelled".into())
            } else {
                ResidentActorWorkbenchError::Resident(ResidentError::Prepared(
                    PreparedRuntimeError::Cancelled,
                ))
            };
            let mut error = workbench_invocation_failure(
                actor,
                WorkbenchExecutionFailure {
                    receipts: vec![item],
                    point: WorkbenchFailurePoint::InputUnit { index: 0 },
                    total: 1,
                    publication: (case == 2).then(|| {
                        WorkbenchPublicationOutcome::RecoveryRequired {
                            detail: "native publication unknown".into(),
                        }
                    }),
                    source,
                },
            );
            if case == 3 {
                error = owned_workbench::retain_cleanup_failure(
                    actor,
                    "native cleanup unknown".into(),
                    WorkbenchFinalizationResult {
                        result: Err(error),
                        cleanup_confirmed: true,
                    },
                )
                .result
                .unwrap_err();
            }
            let change = initial_context_execution_event(&Err(error)).unwrap();
            journal.record_initial_context(actor, change).await.unwrap();
            let observed = journal.initial_context(actor).await.unwrap().unwrap();
            match case {
                0 => assert_eq!(observed.state, InitialContextState::Cancelled),
                1 => assert!(matches!(observed.state, InitialContextState::Failed { .. })),
                _ => assert!(matches!(
                    observed.state,
                    InitialContextState::RecoveryRequired { .. }
                )),
            }
            assert_eq!(observed.receipts, expected_receipts);
            assert!(observed.result.is_none() && observed.publication.is_none());
            drop(journal);
            let reopened =
                crate::ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", root)
                    .await
                    .unwrap();
            assert_eq!(
                reopened.initial_context(actor).await.unwrap(),
                Some(observed)
            );
        }
    }

    #[test]
    fn typed_cancellation_survives_execution_wrappers_without_classifying_error_text() {
        let cancelled = || {
            ResidentActorWorkbenchError::Resident(ResidentError::Prepared(
                PreparedRuntimeError::Cancelled,
            ))
        };
        let sources = [
            cancelled(),
            ResidentActorWorkbenchError::Delivered(ResidentError::Prepared(
                PreparedRuntimeError::Cancelled,
            )),
            ResidentActorWorkbenchError::Resident(ResidentError::Run(
                tidepool_runtime::RuntimeError::Prepared(PreparedRuntimeError::Cancelled),
            )),
            ResidentActorWorkbenchError::InvocationCancelled,
            ResidentActorWorkbenchError::PreparationAdmission(Arc::new(cancelled())),
            ResidentActorWorkbenchError::PrivatePublication {
                actor: ActorRef::first(crate::ActorId(1)),
                scope: ScopeId::ROOT,
                phase: crate::resident_workbench::PrivatePublicationPhase::Publish,
                source: Box::new(cancelled()),
            },
            ResidentActorWorkbenchError::ActivationBindingCommitted {
                binding: tidepool_repr::SessionVarId::from_extract(1),
                source: Box::new(cancelled()),
            },
        ];
        for source in sources {
            assert_eq!(
                source.execution_failure_kind(),
                Some(PreparedFailureKind::Cancelled)
            );
        }
        assert_eq!(
            ResidentActorWorkbenchError::ActorProtocol("prepared execution cancelled".into())
                .execution_failure_kind(),
            None
        );
        assert_eq!(
            ResidentActorWorkbenchError::Resident(ResidentError::Prepared(
                PreparedRuntimeError::MachineNotInstalled
            ))
            .execution_failure_kind(),
            Some(PreparedFailureKind::Rejected)
        );
        for (disposition, expected) in [
            (
                tidepool_codegen::machine_state::MachineDisposition::Reusable,
                PreparedFailureKind::Cancelled,
            ),
            (
                tidepool_codegen::machine_state::MachineDisposition::Unavailable,
                PreparedFailureKind::Integrity,
            ),
        ] {
            let source = ResidentActorWorkbenchError::Resident(ResidentError::Prepared(
                PreparedRuntimeError::Run(
                    tidepool_codegen::prepared_program::ExecutionError::Runtime(
                        tidepool_codegen::machine_state::MachineFailure {
                            cause: tidepool_codegen::host_fns::RuntimeError::Cancelled,
                            disposition,
                        },
                    ),
                ),
            ));
            assert_eq!(source.execution_failure_kind(), Some(expected));
        }
    }
}
