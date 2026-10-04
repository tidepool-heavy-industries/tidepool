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

/// Copy a reader-admitted program into editable test data. This conversion
/// preserves the complete representation but cannot issue compiler authority.
pub fn wire_from_prepared(
    program: &tidepool_repr::execution_schema::PreparedProgram,
) -> WireProgram {
    let view = program.definitions();
    WireProgram {
        envelope: view.envelope().clone(),
        signatures: view.signatures().to_vec(),
        globals: view.globals().to_vec(),
        constructors: view.constructors().to_vec(),
        operations: view.operations().to_vec(),
        expressions: view.expressions().clone(),
        bindings: view.bindings().to_vec(),
        entry: program.entry(),
        types: view.types().to_vec(),
        sites: view.sites().to_vec(),
        constructor_replies: view.constructor_replies().to_vec(),
        json_layout: view.json_layout().copied(),
    }
}
