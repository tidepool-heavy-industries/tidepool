//! Source effects retain the same issued selection as the suspended computation.

use super::*;
use crate::generated::source::{SourceError, SourceReq};
use crate::resident_workbench::ActorRequestSnapshot;

fn source_error(error: crate::SourceInspectionError) -> SourceError {
    match error {
        crate::SourceInspectionError::Unavailable(detail) => SourceError::SourceUnavailable(detail),
        crate::SourceInspectionError::Unreadable(detail) => SourceError::SourceUnreadable(detail),
    }
}

async fn inspect_source<T: Send + 'static>(
    layers: Option<crate::ActorSourceLayerResolver>,
    issued: Option<ActorRequestSnapshot>,
    inspect: impl FnOnce(
            &dyn crate::ActorSourceLayers,
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
        inspect(layers.as_ref(), issued.installation().source()).map_err(source_error)
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
        Ok(SrReloadOutcome::ReloadRejected(..) | SrReloadOutcome::ReloadCancelled(_)) | Err(_) => {
            WorkbenchOperationDisposition::Rejected
        }
        Ok(SrReloadOutcome::ReloadPublicationUnconfirmed(..)) => {
            WorkbenchOperationDisposition::Unknown
        }
    }
}

/// The source owner settles before delivering its answer. A later failure of
/// the original continuation cannot erase an already known publication fact.
async fn resume_source_answer<T, F>(
    answer: Result<T, SourceError>,
    disposition: WorkbenchOperationDisposition,
    resume: impl FnOnce(Result<T, SourceError>) -> F,
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
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        continuation: ResidentHole,
        request: SourceReq,
    ) -> futures_util::future::BoxFuture<'static, commands::CommandResolution> {
        let runner = self.environment.runner.clone();
        let layers = self.environment.source_layers.clone();
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
            SourceReq::SourceReloadWith(also_check) => {
                let operation = issued
                    .as_ref()
                    .and_then(ActorRequestSnapshot::generation)
                    .cloned()
                    .ok_or_else(|| {
                        SourceError::SourceUnavailable(
                            "reload requires an initialized notebook generation".into(),
                        )
                    })
                    .and_then(|generation| {
                        self.coherent_reload_operation(generation, control, also_check)
                    });
                Box::pin(async move {
                    // Keep the complete issued owner through resumption in A,
                    // including after the publication operation releases its copy.
                    let _issued = issued;
                    let result = match operation {
                        Ok(operation) => operation.run().await,
                        Err(error) => Err(error),
                    };
                    let disposition = reload_disposition(&result);
                    // The caller continues in A even when this operation publishes B.
                    resume_source_answer(result, disposition, |answer| {
                        runner.resume_value(context, continuation, answer)
                    })
                    .await
                })
            }
        }
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
                )),
                WorkbenchOperationDisposition::Committed,
            ),
            (
                Ok(SrReloadOutcome::ReloadPublishedWithWarnings(
                    revision("A"),
                    revision("B"),
                    vec!["Helper".into()],
                    vec!["derived projection unavailable".into()],
                )),
                WorkbenchOperationDisposition::Committed,
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
        for (answer, expected) in cases {
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
