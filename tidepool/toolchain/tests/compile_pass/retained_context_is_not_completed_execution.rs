use std::sync::Arc;
use tidepool_toolchain::declaration_join::{CompletedOriginalExecution, OriginalExecutionEvidence};
fn consume(_: CompletedOriginalExecution<'_>) {}
fn use_output(output: &Arc<OriginalExecutionEvidence>) {
    if let Ok(completed) = output.completed() { consume(completed); }
}
fn main() {}
