//! Source effects retain the same issued selection as the suspended computation.

use super::*;
use crate::generated::source::{SourceError, SourceReq};
use crate::resident_workbench::ActorRequestSnapshot;
use crate::HasSourceInspection;

/// Explicit inspection uses the active installation, while retaining which
/// generation the inspecting model request captured. No background polling.
pub(super) async fn refresh_source_observation(
    layers: Option<crate::SourceInspectionServices>,
    installed: Arc<crate::resident_workbench::InstalledToolsState>,
    observation: crate::ActorRuntimeObservationHandle,
    issued: Option<ActorRequestSnapshot>,
) {
    let refresh = observation.source_refresh();
    let token = refresh.token();
    let (Some(layers), Some(active)) = (layers, installed.current_generation()) else {
        observation.finish_source_refresh(
            token,
            Err("no active generation source inspection owner".into()),
        );
        return;
    };
    let source = active.installation().source().clone();
    let active_generation = active.id().get();
    let issued_generation = issued
        .as_ref()
        .and_then(ActorRequestSnapshot::generation)
        .map(|generation| generation.id().get());
    // Filesystem observation needs only immutable issued source evidence. Do
    // not retain the generation's native scope through an uncancellable read.
    let issued_source = issued
        .as_ref()
        .map(|issued| issued.installation().source().clone());
    drop(issued);
    let inspected = tokio::task::spawn_blocking(move || {
        let issued_revision = issued_source
            .as_ref()
            .map(|source| layers.source_inspection().revision(source))
            .transpose()?;
        layers
            .source_inspection()
            .observe(&source)
            .map(|status| (issued_revision, status))
    })
    .await;
    let result = match inspected {
        Ok(Ok((issued_revision, status))) => {
            let active_source_generation = u64::try_from(status.active.generation);
            let disk_source_generation = u64::try_from(status.disk.generation);
            let (Ok(active_source_generation), Ok(disk_source_generation)) =
                (active_source_generation, disk_source_generation)
            else {
                observation.finish_source_refresh(
                    token,
                    Err("source owner returned a negative revision generation".into()),
                );
                return;
            };
            let before: std::collections::BTreeMap<_, _> = status
                .active
                .modules
                .iter()
                .map(|module| (&module.name, &module.digest))
                .collect();
            let after: std::collections::BTreeMap<_, _> = status
                .disk
                .modules
                .iter()
                .map(|module| (&module.name, &module.digest))
                .collect();
            let names: std::collections::BTreeSet<_> =
                before.keys().chain(after.keys()).copied().collect();
            let changed_modules = names
                .into_iter()
                .filter(|name| before.get(*name) != after.get(*name))
                .cloned()
                .collect();
            Ok(crate::SourceLayerDrift {
                issued_identity: issued_revision.map(|revision| revision.identity),
                issued_notebook_generation: issued_generation,
                active_notebook_generation: Some(active_generation),
                active_identity: status.active.identity,
                active_generation: active_source_generation,
                disk_identity: status.disk.identity,
                disk_generation: disk_source_generation,
                changed_modules,
            })
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(format!("source inspection did not complete: {error}")),
    };
    if installed
        .with_current_generation(&active, || observation.finish_source_refresh(token, result))
        .is_none()
    {
        observation.finish_source_refresh(
            token,
            Err("source inspection superseded by generation publication".into()),
        );
    }
}

fn source_error(error: crate::SourceInspectionError) -> SourceError {
    match error {
        crate::SourceInspectionError::Unavailable(detail) => SourceError::SourceUnavailable(detail),
        crate::SourceInspectionError::Unreadable(detail) => SourceError::SourceUnreadable(detail),
    }
}

async fn inspect_source<T: Send + 'static, C: HasSourceInspection + Send + 'static>(
    layers: Option<C>,
    issued: Option<ActorRequestSnapshot>,
    inspect: impl FnOnce(
            &C::SourceInspection,
            &crate::CheckpointSourceLayer,
        ) -> Result<T, crate::SourceInspectionError>
        + Send
        + 'static,
) -> Result<Result<T, SourceError>, ResidentActorWorkbenchError> {
    let Some(layers) = layers else {
        return Ok(Err(SourceError::SourceUnavailable(
            "this actor has no source inspection owner".into(),
        )));
    };
    let Some(issued) = issued else {
        return Ok(Err(SourceError::SourceUnavailable(
            "this execution has no issued source snapshot".into(),
        )));
    };
    tokio::task::spawn_blocking(move || {
        inspect(layers.source_inspection(), issued.installation().source()).map_err(source_error)
    })
    .await
    .map_err(|error| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "issued source inspection did not complete: {error}"
        ))
    })
}

pub(super) fn reload_disposition(
    answer: &Result<tidepool_bridge_effects::SrReloadOutcome, SourceError>,
) -> WorkbenchOperationDisposition {
    use tidepool_bridge_effects::SrReloadOutcome;
    match answer {
        Ok(SrReloadOutcome::ReloadUnchanged(_)) => WorkbenchOperationDisposition::Read,
        Ok(
            SrReloadOutcome::ReloadPublished(..) | SrReloadOutcome::ReloadPublishedWithWarnings(..),
        ) => WorkbenchOperationDisposition::Committed,
        Ok(
            SrReloadOutcome::ReloadRejected(..)
            | SrReloadOutcome::ReloadRefused(..)
            | SrReloadOutcome::ReloadCancelled(_),
        )
        | Err(_) => WorkbenchOperationDisposition::Rejected,
        Ok(SrReloadOutcome::ReloadPublicationUnconfirmed(..)) => {
            WorkbenchOperationDisposition::Unknown
        }
        Ok(SrReloadOutcome::ReloadRetirement(receipt)) => {
            use tidepool_bridge_effects::{
                RlCleanupOutcome, RlOperationalSettlement, RlRetirementState, RlStopOutcome,
            };
            if matches!(
                receipt.settlement,
                RlOperationalSettlement::ReloadOperationalUnsettled(..)
            ) {
                return WorkbenchOperationDisposition::Unknown;
            }
            match &receipt.state {
                None | Some(RlRetirementState::ReloadRetirementFenced(_)) => {
                    WorkbenchOperationDisposition::Unknown
                }
                Some(RlRetirementState::ReloadBPublished(..)) => {
                    WorkbenchOperationDisposition::Committed
                }
                Some(RlRetirementState::ReloadAReopened) => {
                    if receipt.outcomes.iter().any(|outcome| {
                        matches!(&outcome.stop, RlStopOutcome::ReloadStopConfirmed)
                            || matches!(&outcome.cleanup, RlCleanupOutcome::ReloadCleanupConfirmed)
                    }) {
                        WorkbenchOperationDisposition::Committed
                    } else {
                        WorkbenchOperationDisposition::Rejected
                    }
                }
            }
        }
    }
}

/// The source owner settles before delivering its answer. A later failure of
/// the original continuation cannot erase an already known publication fact.
async fn resume_source_answer<T, E, F>(
    answer: Result<T, E>,
    disposition: WorkbenchOperationDisposition,
    resume: impl FnOnce(Result<T, E>) -> F,
) -> commands::CommandResolution
where
    F: std::future::Future<Output = Result<ResidentOutcome, ResidentActorWorkbenchError>>,
{
    commands::CommandResolution::settled(disposition, resume(answer).await)
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn prepare_source_effect(
        &self,
        context: ActorSessionContext,
        issued: Option<ActorRequestSnapshot>,
        continuation: ResidentHole,
        request: SourceReq,
    ) -> futures_util::future::BoxFuture<'static, commands::CommandResolution> {
        let runner = self.environment.runner.clone();
        let layers = self.environment.source_inspection.clone();
        match request {
            SourceReq::SourceReadModule(name) => Box::pin(async move {
                let _issued = issued.clone();
                let answer = match inspect_source(layers, issued, move |owner, source| {
                    owner.read_from(source, &name)
                })
                .await
                {
                    Ok(answer) => answer,
                    Err(error) => return commands::CommandResolution::from_outcome(Err(error)),
                };
                let disposition = if answer.is_ok() {
                    WorkbenchOperationDisposition::Read
                } else {
                    WorkbenchOperationDisposition::Rejected
                };
                resume_source_answer(answer, disposition, |answer| {
                    runner.resume_value(context, continuation, answer)
                })
                .await
            }),
            SourceReq::SourceStatusWith => Box::pin(async move {
                let _issued = issued.clone();
                let answer =
                    match inspect_source(layers, issued, |owner, source| owner.observe(source))
                        .await
                    {
                        Ok(answer) => answer,
                        Err(error) => return commands::CommandResolution::from_outcome(Err(error)),
                    };
                let disposition = if answer.is_ok() {
                    WorkbenchOperationDisposition::Read
                } else {
                    WorkbenchOperationDisposition::Rejected
                };
                resume_source_answer(answer, disposition, |answer| {
                    runner.resume_value(context, continuation, answer)
                })
                .await
            }),
        }
    }
    pub(super) fn prepare_reload_effect(
        &self,
        kernel: KernelContext,
        coordinator: Option<Arc<InvocationWork>>,
        private: Option<Arc<crate::resident_workbench::ExecutionPrivateScope>>,
        context: ActorSessionContext,
        issued: Option<ActorRequestSnapshot>,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        continuation: ResidentHole,
        request: crate::generated::reload::ReloadReq,
        authorized: bool,
        diagnostic: Option<WorkbenchOperationDiagnosticOwner>,
    ) -> futures_util::future::BoxFuture<'static, commands::CommandResolution> {
        use crate::generated::reload::{ReloadError, ReloadReq};
        let runner = self.environment.runner.clone();
        let return_control = control.clone();
        let ReloadReq::ReloadSourceWith(options) = request;
        let operation = if !authorized {
            Err(SourceError::SourceUnavailable(
                "reload requires an exact synchronous Reload invocation".into(),
            ))
        } else if options.force {
            Err(SourceError::SourceUnavailable(
                "forced generation retirement is not supported".into(),
            ))
        } else {
            diagnostic
                .ok_or_else(|| {
                    SourceError::SourceUnavailable(
                        "reload has no actual admitted operation diagnostic owner".into(),
                    )
                })
                .and_then(|diagnostic| {
                    let coordinator = coordinator.ok_or_else(|| {
                        SourceError::SourceUnavailable(
                            "reload has no actual admitted invocation owner".into(),
                        )
                    })?;
                    issued
                        .as_ref()
                        .and_then(ActorRequestSnapshot::generation)
                        .cloned()
                        .ok_or_else(|| {
                            SourceError::SourceUnavailable(
                                "reload requires an initialized notebook generation".into(),
                            )
                        })
                        .and_then(|generation| {
                            self.coherent_reload_operation(
                                kernel,
                                coordinator,
                                generation,
                                control,
                                options.also_check,
                                options.policy,
                                diagnostic,
                            )
                        })
                })
        };
        Box::pin(async move {
            // Publication B and continuation A retain their separate immutable owners.
            let _issued = issued;
            let answer = match operation {
                Ok(operation) => {
                    let checked_return = runner
                        .capture_checked_return_owner(
                            context.clone(),
                            private,
                            return_control.clone(),
                            continuation.clone(),
                        )
                        .await;
                    match checked_return {
                        Ok(owner) => operation.run(owner).await,
                        Err(error) => Err(SourceError::SourceUnavailable(error.to_string())),
                    }
                }
                Err(error) => Err(error),
            };
            let (answer, supersession) = match answer {
                Ok((answer, supersession)) => (Ok(answer), supersession),
                Err(error) => (Err(error), None),
            };
            let disposition = reload_disposition(&answer);
            let answer = answer.map_err(|error| match error {
                SourceError::SourceUnavailable(detail) => ReloadError::ReloadUnavailable(detail),
                SourceError::SourceUnreadable(detail) => ReloadError::ReloadUnreadable(detail),
            });
            let mut resolution = resume_source_answer(answer, disposition, |answer| {
                let supersession = supersession.clone();
                async move {
                    match supersession {
                        Some(supersession) => {
                            runner
                                .resume_superseded_value(
                                    context,
                                    continuation,
                                    answer,
                                    supersession,
                                    return_control,
                                )
                                .await
                        }
                        None => runner.resume_value(context, continuation, answer).await,
                    }
                }
            })
            .await;
            resolution.supersession = supersession;
            resolution
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_bridge_effects::{SrReloadOutcome, SrRevision};

    fn revision(name: &str) -> SrRevision {
        SrRevision {
            identity: name.into(),
            generation: 1,
            modules: Vec::new(),
        }
    }

    #[tokio::test]
    async fn reload_receipt_retains_owner_settlement_when_continuation_fails() {
        // These are ordinary Haskell answers, not generation/publication authority.
        let cases = [
            (
                Ok(SrReloadOutcome::ReloadUnchanged(revision("A"))),
                WorkbenchOperationDisposition::Read,
            ),
            (
                Ok(SrReloadOutcome::ReloadPublished(
                    revision("A"),
                    revision("B"),
                    vec!["Helper".into()],
                    tidepool_bridge_effects::RlSurfaceDiff {
                        added: vec![],
                        removed: vec![],
                        changed: vec![],
                    },
                )),
                WorkbenchOperationDisposition::Committed,
            ),
            (
                Ok(SrReloadOutcome::ReloadPublishedWithWarnings(
                    revision("A"),
                    revision("B"),
                    vec!["Helper".into()],
                    vec!["derived projection unavailable".into()],
                    tidepool_bridge_effects::RlSurfaceDiff {
                        added: vec![],
                        removed: vec![],
                        changed: vec![],
                    },
                )),
                WorkbenchOperationDisposition::Committed,
            ),
            (
                Ok(SrReloadOutcome::ReloadRefused(
                    revision("A"),
                    revision("B"),
                    tidepool_bridge_effects::RlRefusal {
                        category: tidepool_bridge_effects::RlRefusalCategory::SurfaceIncompatible,
                        detail: "opt-in required".into(),
                        surface: tidepool_bridge_effects::RlSurfaceDiff {
                            added: vec!["new_tool".into()],
                            removed: vec![],
                            changed: vec![],
                        },
                    },
                )),
                WorkbenchOperationDisposition::Rejected,
            ),
            (
                Ok(SrReloadOutcome::ReloadRejected(
                    revision("A"),
                    revision("B"),
                    "type error".into(),
                )),
                WorkbenchOperationDisposition::Rejected,
            ),
            (
                Ok(SrReloadOutcome::ReloadCancelled(revision("A"))),
                WorkbenchOperationDisposition::Rejected,
            ),
            (
                Ok(SrReloadOutcome::ReloadPublicationUnconfirmed(
                    revision("A"),
                    "native acknowledgement lost".into(),
                )),
                WorkbenchOperationDisposition::Unknown,
            ),
            (
                Err(SourceError::SourceUnavailable("captured-only actor".into())),
                WorkbenchOperationDisposition::Rejected,
            ),
            (
                Err(SourceError::SourceUnreadable(
                    "candidate source unavailable".into(),
                )),
                WorkbenchOperationDisposition::Rejected,
            ),
        ];
        let retirement = |state, settlement| {
            Ok(SrReloadOutcome::ReloadRetirement(
                tidepool_bridge_effects::RlRetirementReceipt {
                    previous: revision("A"),
                    candidate: revision("B"),
                    operation: None,
                    state,
                    blockers: vec![],
                    outcomes: vec![],
                    surface: tidepool_bridge_effects::RlSurfaceDiff {
                        added: vec![],
                        removed: vec![],
                        changed: vec![],
                    },
                    primary: Some(tidepool_bridge_effects::RlCause {
                        kind: tidepool_bridge_effects::RlCauseKind::ReloadPublicationFailed,
                        detail: "a completed failure can have all obligations settled".into(),
                    }),
                    secondary: vec![],
                    settlement,
                },
            ))
        };
        use tidepool_bridge_effects::{
            RlObligationObservation as Observation, RlOperationalSettlement as Settlement,
            RlRetirementObligation as Obligation, RlRetirementState as State,
        };
        let explicit_settlement = [
            (
                retirement(
                    Some(State::ReloadAReopened),
                    Settlement::ReloadOperationalSettled,
                ),
                WorkbenchOperationDisposition::Rejected,
            ),
            (
                retirement(
                    Some(State::ReloadBPublished(vec![], vec![])),
                    Settlement::ReloadOperationalSettled,
                ),
                WorkbenchOperationDisposition::Committed,
            ),
            (
                retirement(
                    Some(State::ReloadAReopened),
                    Settlement::ReloadOperationalUnsettled(
                        Obligation::ReloadPhysicalCleanupObligation(
                            Observation::ReloadObligationUnconfirmed,
                        ),
                        vec![],
                    ),
                ),
                WorkbenchOperationDisposition::Unknown,
            ),
            (
                retirement(
                    Some(State::ReloadBPublished(vec![], vec![])),
                    Settlement::ReloadOperationalUnsettled(
                        Obligation::ReloadOriginalRetirementObligation(
                            Observation::ReloadObligationRefused,
                        ),
                        vec![],
                    ),
                ),
                WorkbenchOperationDisposition::Unknown,
            ),
            (
                retirement(None, Settlement::ReloadOperationalSettled),
                WorkbenchOperationDisposition::Unknown,
            ),
        ];
        for (answer, expected) in cases.into_iter().chain(explicit_settlement) {
            let disposition = reload_disposition(&answer);
            assert_eq!(disposition, expected);
            let delivered = format!("{answer:?}");
            let resolution = resume_source_answer(answer, disposition, |answer| async move {
                assert_eq!(format!("{answer:?}"), delivered);
                Err(ResidentActorWorkbenchError::ActorProtocol(
                    "original A continuation failed after owner settlement".into(),
                ))
            })
            .await;
            assert!(resolution.outcome.is_err());
            assert_eq!(resolution.disposition, expected);
            assert!(
                resolution.supersession.is_none(),
                "ordinary reload answers cannot issue generation supersession"
            );
            assert_eq!(
                scoped_operation_disposition(
                    WorkbenchOperationDisposition::Committed,
                    resolution.disposition
                ),
                expected
            );
            assert!(resolution.started_job.is_none());
            assert!(resolution.retained_job_binding.is_none());
        }
    }
    #[tokio::test]
    async fn ordinary_effect_failure_has_no_source_publication_claim() {
        let resolution = commands::CommandResolution::ordinary(Box::pin(async {
            Err(ResidentActorWorkbenchError::ActorProtocol(
                "ordinary effect has no settlement evidence".into(),
            ))
        }))
        .await;
        assert_eq!(
            resolution.disposition,
            WorkbenchOperationDisposition::Unknown
        );
        assert!(resolution.outcome.is_err());
        assert!(resolution.started_job.is_none());
        assert!(resolution.retained_job_binding.is_none());
    }
}
