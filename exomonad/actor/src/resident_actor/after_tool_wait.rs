//! One after-tool entry retains its admitted tools and its original deadline.

use super::*;
use tidepool_runtime::session::workbench::WorkbenchToolCall;

pub(super) struct AfterToolFrame {
    tools: crate::InstalledToolLease,
    dispatch: Arc<RootCustody>,
    call: WorkbenchToolCall,
    output: String,
    ordinal: u64,
    handle: String,
    provenance: String,
    revision: String,
    entered: tokio::time::Instant,
    deadline: tokio::time::Instant,
    remaining_display_budget: usize,
}

/// The actor reserves this slot ordinal and selects its original tool lease
/// before capture. The frame is moved through successor tasks, never recaptured.
pub(super) fn capture(
    tools: crate::InstalledToolLease,
    dispatch: Arc<RootCustody>,
    call: WorkbenchToolCall,
    output: String,
    ordinal: u64,
    remaining: usize,
) -> AfterToolFrame {
    let retained = tools
        .tools()
        .expect("after-tool capture requires the original installed dispatcher");
    let provenance = retained.provenance();
    let revision = retained.revision.clone().unwrap_or_else(|| "(run)".into());
    let entered = tokio::time::Instant::now();
    let deadline = entered + crate::after_tool::wait();
    AfterToolFrame {
        tools,
        dispatch,
        call,
        output,
        ordinal,
        handle: format!("toolResult{ordinal}"),
        provenance,
        revision,
        entered,
        deadline,
        remaining_display_budget: remaining,
    }
}

impl AfterToolFrame {
    pub(super) fn tools(&self) -> &crate::InstalledToolLease {
        &self.tools
    }

    pub(super) fn dispatch(&self) -> &Arc<RootCustody> {
        &self.dispatch
    }

    pub(super) fn call(&self) -> &WorkbenchToolCall {
        &self.call
    }

    pub(super) fn output(&self) -> &str {
        &self.output
    }

    pub(super) fn ordinal(&self) -> u64 {
        self.ordinal
    }

    pub(super) fn handle(&self) -> &str {
        &self.handle
    }

    pub(super) fn provenance(&self) -> &str {
        &self.provenance
    }

    pub(super) fn revision(&self) -> &str {
        &self.revision
    }

    pub(super) fn entered(&self) -> tokio::time::Instant {
        self.entered
    }

    pub(super) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    pub(super) fn remaining_display_budget(&self) -> usize {
        self.remaining_display_budget
    }

    pub(super) fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "call": { "name": self.call.name, "arguments": self.call.arguments },
            "result": {
                "name": self.call.name,
                "handle": self.handle,
                "ordinal": self.ordinal,
                "output": self.output,
            },
        })
    }
}

pub(super) struct AfterToolPrepared {
    frame: AfterToolFrame,
    step: Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>,
}

impl AfterToolPrepared {
    pub(super) fn into_parts(
        self,
    ) -> (
        AfterToolFrame,
        Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>,
    ) {
        (self.frame, self.step)
    }
}

/// Only the original workbench's native begin runs here. The actor applies the
/// returned step under its existing fence and owns effect/timeout settlement.
pub(super) async fn prepare<H, O>(
    frame: AfterToolFrame,
    workbench: &crate::ResidentActorWorkbench<H, O>,
    context: ActorSessionContext,
) -> AfterToolPrepared
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let step = if frame.tools.actor() != context.actor {
        Err(ResidentActorWorkbenchError::ActorProtocol(
            "after-tool lease belongs to another actor incarnation".into(),
        ))
    } else if !frame
        .tools
        .tools()
        .is_some_and(|tools| Arc::ptr_eq(&tools.dispatch, &frame.dispatch))
    {
        Err(ResidentActorWorkbenchError::ActorProtocol(
            "after-tool dispatcher differs from its retained installation".into(),
        ))
    } else {
        workbench
            .begin_after_tool(
                context,
                Arc::clone(&frame.dispatch),
                frame.call.name.clone(),
                frame.payload(),
            )
            .await
    };
    AfterToolPrepared { frame, step }
}
