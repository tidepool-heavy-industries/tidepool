use std::sync::Arc;
use tidepool_toolchain::activation_preview::OriginalDisplayContext;
use tidepool_toolchain::checked_cell::ExactHostBindingPrototype;
use tidepool_toolchain::declaration_join::OriginalExecutionEvidence;
use tidepool_toolchain::CompileError;

fn issue(
    prototype: &ExactHostBindingPrototype,
    evidence: &Arc<OriginalExecutionEvidence>,
) -> Result<Arc<OriginalDisplayContext>, CompileError> {
    OriginalDisplayContext::issue(prototype, evidence.completed().map_err(CompileError::from)?)
}

fn main() {}
