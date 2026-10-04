//! Current typed data for tests of structural validation and linking.

use tidepool_repr::execution_schema::{testing, GlobalDecl, RuntimeRep, SignatureId, WireProgram};

/// A valid target plus a declared callable import, without compiler provenance.
pub fn callable_import_program() -> WireProgram {
    let mut wire = testing::wire_program();
    wire.globals.push(GlobalDecl {
        identity: testing::identity("Imported", "callable"),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(SignatureId(0)),
        required_evaluated: true,
        required_generation: None,
    });
    wire
}
