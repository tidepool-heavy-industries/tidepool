use std::sync::Arc;
use tidepool_toolchain::compiler_world::CompilerWorld;
use tidepool_toolchain::declaration_join::SelectedTypeInterfaces;
use tidepool_toolchain::CompileError;

fn extend(
    world: &Arc<CompilerWorld>,
    interfaces: &[Arc<SelectedTypeInterfaces>],
) -> Result<Arc<CompilerWorld>, CompileError> {
    world.with_type_interfaces(interfaces)
}
fn main() {}
