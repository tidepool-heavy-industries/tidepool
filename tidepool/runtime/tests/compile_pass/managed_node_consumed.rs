use tidepool_codegen::prepared_program::{ManagedField, ManagedNode};
fn consume(node: ManagedNode<'_>) {
    let _ = [ManagedField::Borrow(&node), ManagedField::Borrow(&node)];
    let _ = ManagedField::Node(node);
}
fn main() {}
