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
    ) -> futures_util::future::BoxFuture<
        'static,
        Result<ResidentOutcome, ResidentActorWorkbenchError>,
    > {
        let runner = self.environment.runner.clone();
        let layers = self.environment.source_layers.clone();
        match request {
            SourceReq::SourceReadModule(name) => Box::pin(async move {
                let result = inspect_source(layers, issued, move |owner, source| {
                    owner.read_from(source, &name)
                })
                .await?;
                runner.resume_value(context, continuation, result).await
            }),
            SourceReq::SourceStatusWith => Box::pin(async move {
                let result =
                    inspect_source(layers, issued, |owner, source| owner.observe(source)).await?;
                runner.resume_value(context, continuation, result).await
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
                    let result = match operation {
                        Ok(operation) => operation.run().await,
                        Err(error) => Err(error),
                    };
                    // The caller continues in A even when this operation publishes B.
                    runner.resume_value(context, continuation, result).await
                })
            }
        }
    }
}
