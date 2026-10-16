use tidepool_toolchain::artifact_inventory::{ArtifactBindingNode, NativeGroupBinding};
fn node(group: NativeGroupBinding) -> ArtifactBindingNode {
    ArtifactBindingNode::Group(group)
}
fn main() {}
