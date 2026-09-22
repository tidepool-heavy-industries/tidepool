use tidepool_harness::selfharness::operator::{FormShape, OperatorGate};

struct MinimalGate;

impl OperatorGate for MinimalGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        serde_json::Value::Null
    }
}

#[test]
fn node_lifecycle_defaults_are_no_ops() {
    let gate = MinimalGate;

    gate.node_seeded("root/child", "authored brief");
    gate.node_finalized("root/child", r#"{"answer":42}"#);
    gate.node_failed("root/child", "provider exited");
}
