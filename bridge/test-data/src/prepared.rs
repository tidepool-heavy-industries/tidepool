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

/// Closed constructor data for paired metadata/IR structural checks.
pub fn constructor_program() -> WireProgram {
    use tidepool_repr::execution_schema::{
        Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        ResultContract, ScalarLiteral,
    };
    let mut wire = testing::wire_program();
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("Fixture", "Box"),
        host_id: tidepool_repr::DataConId(901),
        family: testing::identity("Fixture", "Box"),
        result_rep: RuntimeRep::LiftedRef,
        field_reps: vec![RuntimeRep::Int(64)],
        strict_fields: vec![false],
        layout: CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::Int(64),
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![false],
        },
        tag: 1,
        family_size: 1,
    });
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.expressions.nodes[0] = ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: 42_i64.to_be_bytes().to_vec(),
        })],
    };
    wire
}
