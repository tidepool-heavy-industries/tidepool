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
        types: view.types().clone(),
        sites: view.sites().to_vec(),
        constructor_replies: view.constructor_replies().to_vec(),
        json_layout: view.json_layout().copied(),
    }
}

fn constructor_identity(
    module: &str,
    occurrence: &str,
) -> tidepool_repr::execution_schema::SymbolIdentity {
    let mut identity = testing::identity(module, occurrence);
    identity.namespace = "constructor".into();
    identity
}
fn type_identity(
    module: &str,
    occurrence: &str,
) -> tidepool_repr::execution_schema::SymbolIdentity {
    let mut identity = testing::identity(module, occurrence);
    identity.namespace = "type".into();
    identity
}

/// Closed constructor data for paired metadata/IR structural checks.
pub fn constructor_program() -> WireProgram {
    use tidepool_repr::execution_schema::{
        Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        ResultContract, ScalarLiteral,
    };
    let mut wire = testing::wire_program();
    wire.constructors.push(ConstructorDecl {
        identity: super::prepared::constructor_identity("Fixture", "Box"),
        host_id: tidepool_repr::DataConId(901),
        family: super::prepared::type_identity("Fixture", "Box"),
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

pub use testing::{closed_type_graph, closed_type_roots, type_graph};

/// A current typed program with a closed Text root for reader-work controls.
pub fn text_type_program() -> WireProgram {
    let mut wire = testing::wire_program();
    wire.types = closed_type_graph(
        testing::identity("Types", "Text"),
        tidepool_repr::type_graph::DeclarationForm::Text,
    );
    wire
}
