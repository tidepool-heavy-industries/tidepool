use std::sync::Arc;
use tidepool_toolchain::activation_preview::OriginalDisplayContext;
use tidepool_toolchain::checked_cell::ExactHostBindingPrototype;
use tidepool_toolchain::declaration_join::SelectedTypeInterfaces;
use tidepool_toolchain::CompileError;

fn issue(
    prototype: &ExactHostBindingPrototype,
    interfaces: &SelectedTypeInterfaces,
) -> Result<Arc<OriginalDisplayContext>, CompileError> {
    OriginalDisplayContext::issue(prototype, interfaces)
}
fn main() {}
