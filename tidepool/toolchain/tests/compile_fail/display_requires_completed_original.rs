use std::sync::Arc;
use tidepool_toolchain::activation_preview::OriginalDisplayContext;
use tidepool_toolchain::checked_cell::ExactHostBindingPrototype;
use tidepool_toolchain::declaration_join::ExactDeclarationContext;
use tidepool_toolchain::CompileError;

fn issue(
    prototype: &ExactHostBindingPrototype,
    context: &ExactDeclarationContext,
) -> Result<Arc<OriginalDisplayContext>, CompileError> {
    OriginalDisplayContext::issue(prototype, context)
}

fn main() {}
