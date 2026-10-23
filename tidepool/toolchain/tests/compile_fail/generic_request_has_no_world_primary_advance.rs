use std::sync::Arc;
use tidepool_toolchain::declaration_join::{ExactCompileContext, SelectedTypeInterfaces};
use tidepool_toolchain::CompileError;

fn extend(
    owner: &Arc<ExactCompileContext>,
    interfaces: &[Arc<SelectedTypeInterfaces>],
) -> Result<Arc<ExactCompileContext>, CompileError> {
    owner.with_type_interfaces(interfaces)
}
fn main() {}
