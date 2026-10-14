use tidepool_codegen::prepared_program::{ManagedField, ManagedNode};
fn consume(node: ManagedNode<'_>) {
    let _ = [ManagedField::Node(node.clone()), ManagedField::Node(node)];
}
fn main() {}
