//! Inspection uses the original workbench and exact actor session context.

use super::*;
use crate::status_tool::StatusDiscovery;
use crate::ResidentActorWorkbench;

#[derive(Debug)]
pub(super) enum InspectionRequest {
    Recovery,
    Bindings,
    Live,
}

#[derive(Debug)]
pub(super) enum InspectionResult {
    Rendered(String),
    Live(Vec<tidepool_runtime::session::WorkbenchBinding>),
}

pub(super) async fn inspect<H, O>(
    workbench: &ResidentActorWorkbench<H, O>,
    context: ActorSessionContext,
    request: InspectionRequest,
) -> Result<InspectionResult, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    match request {
        InspectionRequest::Recovery => workbench
            .status_discovery(context, StatusDiscovery::Recovery)
            .await
            .map(InspectionResult::Rendered),
        InspectionRequest::Bindings => workbench
            .status_discovery(context, StatusDiscovery::Bindings)
            .await
            .map(InspectionResult::Rendered),
        InspectionRequest::Live => workbench
            .live_bindings(context)
            .await
            .map(InspectionResult::Live),
    }
}

#[cfg(test)]
#[path = "inspection_wait_tests.rs"]
mod tests;
