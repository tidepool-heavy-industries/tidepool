//! Current typed data for tests of structural validation and linking.

use tidepool_repr::execution_schema::{
    testing, GlobalDecl, RuntimeRep, SignatureId, SymbolIdentity, WireProgram,
};

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

/// Freeze structural graph fixtures through the same owning validator. This
/// describes type metadata and cannot confer compiler or execution authority.
pub fn type_graph(
    nodes: Vec<tidepool_repr::type_graph::TypeNode>,
    edges: &[(u32, u32, tidepool_repr::type_graph::TypeEdge)],
    constructors: &[tidepool_repr::execution_schema::ConstructorDecl],
) -> Result<
    std::sync::Arc<tidepool_repr::type_graph::TypeGraph>,
    tidepool_repr::type_graph::TypeGraphError,
> {
    use tidepool_repr::type_graph::{GraphLimits, GraphStorage, TypeGraph, TypeNodeId};
    let mut graph = GraphStorage::with_capacity(nodes.len(), edges.len());
    for node in nodes {
        graph.add_node(node);
    }
    for &(source, target, role) in edges {
        if let Some(invalid) = [source, target]
            .into_iter()
            .find(|index| *index as usize >= graph.node_count())
        {
            return Err(tidepool_repr::type_graph::TypeGraphError::InvalidReference(
                invalid as usize,
            ));
        }
        graph.add_edge(
            TypeNodeId::new(source as usize),
            TypeNodeId::new(target as usize),
            role,
        );
    }
    TypeGraph::validate(graph, constructors, GraphLimits::default()).map(std::sync::Arc::new)
}

/// Ordered closed nominal roots for site-routing fixtures. All endpoint IDs
/// precede expression nodes; repeated nominal declarations share their owner.
pub fn closed_type_roots(
    declarations: &[(SymbolIdentity, tidepool_repr::type_graph::DeclarationForm)],
) -> std::sync::Arc<tidepool_repr::type_graph::TypeGraph> {
    use tidepool_repr::type_graph::{RootDomain, SyntaxRestriction, TypeEdge, TypeNode};
    let mut nodes = declarations
        .iter()
        .map(|(identity, _)| TypeNode::Root {
            domain: RootDomain::Closed,
            binders: vec![],
            rendered: identity.occurrence.clone(),
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut owners = std::collections::BTreeMap::new();
    for (root, (identity, form)) in declarations.iter().enumerate() {
        let mut identity = identity.clone();
        identity.namespace = "type".into();
        identity.record_parent = None;
        let declaration = match owners.get(&identity) {
            Some((original_form, node)) => {
                assert_eq!(
                    original_form, form,
                    "one nominal fixture owner cannot have conflicting declarations"
                );
                *node
            }
            None => {
                let node = nodes.len() as u32;
                owners.insert(identity.clone(), (form.clone(), node));
                nodes.push(TypeNode::Declaration {
                    identity,
                    parameters: vec![],
                    form: form.clone(),
                    restriction: SyntaxRestriction::None,
                });
                node
            }
        };
        let expression = nodes.len() as u32;
        nodes.push(TypeNode::NominalApplication);
        edges.extend([
            (root as u32, expression, TypeEdge::Body),
            (expression, declaration, TypeEdge::Head),
        ]);
    }
    type_graph(nodes, &edges, &[]).expect("closed structural type fixtures")
}

/// One closed nominal root for site-routing fixtures, with no physical fields.
pub fn closed_type_graph(
    identity: SymbolIdentity,
    form: tidepool_repr::type_graph::DeclarationForm,
) -> std::sync::Arc<tidepool_repr::type_graph::TypeGraph> {
    closed_type_roots(&[(identity, form)])
}
