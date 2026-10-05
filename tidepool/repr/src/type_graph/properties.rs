//! Bounded representation-only graph properties. The oracle uses an edge list
//! and relation refinement, never the production cursor or identity walk.

use super::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct Family {
    parameters: Vec<u8>,
    constructors: Vec<Vec<u8>>,
}

fn cases() -> impl Strategy<Value = Vec<Family>> {
    prop::collection::vec(
        (
            prop::collection::vec(0_u8..4, 1..3),
            prop::collection::vec(prop::collection::vec(0_u8..30, 0..4), 1..3),
        )
            .prop_map(|(parameters, constructors)| Family {
                parameters,
                constructors,
            }),
        1..4,
    )
}

#[derive(Clone, Debug)]
struct Model {
    nodes: Vec<TypeNode>,
    edges: Vec<(usize, usize, TypeEdge)>,
    inventory: Vec<ConstructorDecl>,
    root: usize,
    declaration: usize,
    argument: usize,
}

impl Model {
    fn node(&mut self, node: TypeNode) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        index
    }

    fn edge(&mut self, source: usize, target: usize, role: TypeEdge) {
        self.edges.push((source, target, role));
    }

    fn storage(&self) -> GraphStorage {
        let mut graph = GraphStorage::new();
        for node in &self.nodes {
            graph.add_node(node.clone());
        }
        for &(source, target, role) in &self.edges {
            graph.add_edge(TypeNodeId::new(source), TypeNodeId::new(target), role);
        }
        graph
    }

    fn publish(&self) -> TypeGraph {
        TypeGraph::validate(self.storage(), &self.inventory, GraphLimits::default())
            .expect("constructive graph must validate")
    }

    fn adjacency(&self) -> Vec<Vec<(TypeEdge, usize)>> {
        let mut edges = vec![Vec::new(); self.nodes.len()];
        for &(source, target, role) in &self.edges {
            edges[source].push((role, target));
        }
        for edges in &mut edges {
            edges.sort_by_key(|&(role, _)| role_order(role));
        }
        edges
    }
}

// Explicit wire-role order keeps the oracle independent of TypeEdge::Ord and
// publication's sort. Distinct slots to the same target remain distinct edges.
fn role_order(role: TypeEdge) -> (u8, u32) {
    match role {
        TypeEdge::BinderKind(i) => (0, i),
        TypeEdge::Body => (1, 0),
        TypeEdge::Head => (2, 0),
        TypeEdge::Argument(i) => (3, i),
        TypeEdge::Function => (4, 0),
        TypeEdge::ApplyArgument => (5, 0),
        TypeEdge::Multiplicity => (6, 0),
        TypeEdge::Domain => (7, 0),
        TypeEdge::Codomain => (8, 0),
        TypeEdge::Kind => (9, 0),
        TypeEdge::Constructor(i) => (10, i),
        TypeEdge::Field { ordinal, .. } => (11, ordinal),
        TypeEdge::AliasRhs => (12, 0),
    }
}

fn metadata(node: &TypeNode) -> TypeNode {
    let mut node = node.clone();
    match &mut node {
        TypeNode::Root { rendered, .. } => rendered.clear(),
        TypeNode::Declaration {
            form: DeclarationForm::Opaque { reason, .. },
            ..
        } => reason.clear(),
        TypeNode::ConstructorTemplate { constructor, .. } => *constructor = ConstructorId(0),
        _ => {}
    }
    node
}

// Greatest fixed point of matching labels and ordered successors. Refinement
// starts with every metadata-compatible pair and removes unsupported pairs;
// cycles and sharing need no recursion, visited-pair walk, or tree expansion.
fn relation(first: &Model, second: &Model) -> Vec<Vec<bool>> {
    let first_edges = first.adjacency();
    let second_edges = second.adjacency();
    let mut related: Vec<Vec<bool>> = first
        .nodes
        .iter()
        .map(|a| {
            second
                .nodes
                .iter()
                .map(|b| metadata(a) == metadata(b))
                .collect()
        })
        .collect();
    loop {
        let mut removals = Vec::new();
        for (a, successors) in first_edges.iter().enumerate() {
            for (b, other_successors) in second_edges.iter().enumerate() {
                if related[a][b]
                    && (successors.len() != other_successors.len()
                        || successors.iter().zip(other_successors).any(
                            |(&(role, target), &(other_role, other_target))| {
                                role != other_role || !related[target][other_target]
                            },
                        ))
                {
                    removals.push((a, b));
                }
            }
        }
        if removals.is_empty() {
            return related;
        }
        for (a, b) in removals {
            related[a][b] = false;
        }
    }
}

fn build(families: &[Family]) -> Model {
    let mut model = Model {
        nodes: Vec::new(),
        edges: Vec::new(),
        inventory: Vec::new(),
        root: 0,
        declaration: 0,
        argument: 0,
    };
    let kind = model.node(TypeNode::Literal(TypeLiteral::Symbol("kind".into())));
    let declarations: Vec<_> = families
        .iter()
        .enumerate()
        .map(|(i, family)| {
            model.node(TypeNode::Declaration {
                identity: tests::identity(&format!("Family{i}"), "type"),
                parameters: family
                    .parameters
                    .iter()
                    .map(|flag| match flag {
                        0 => ParameterFlag::NamedRequired,
                        1 => ParameterFlag::NamedSpecified,
                        2 => ParameterFlag::NamedInferred,
                        _ => ParameterFlag::AnonymousVisible,
                    })
                    .collect(),
                form: DeclarationForm::Data,
                restriction: SyntaxRestriction::None,
            })
        })
        .collect();
    model.declaration = declarations[0];
    for (family_index, family) in families.iter().enumerate() {
        let declaration = declarations[family_index];
        for parameter in 0..family.parameters.len() {
            model.edge(declaration, kind, TypeEdge::BinderKind(parameter as u32));
        }
        let bounds: Vec<_> = (0..family.parameters.len())
            .map(|i| model.node(TypeNode::Bound(i as u32)))
            .collect();
        for (constructor_index, fields) in family.constructors.iter().enumerate() {
            let reps: Vec<_> = fields
                .iter()
                .map(|field| match field % 3 {
                    0 => RuntimeRep::LiftedRef,
                    1 => RuntimeRep::Int(64),
                    _ => RuntimeRep::Word(32),
                })
                .collect();
            let mut physical = tests::physical(reps.clone());
            physical.identity = tests::identity(
                &format!("Constructor{family_index}_{constructor_index}"),
                "data",
            );
            physical.family = tests::identity(&format!("Family{family_index}"), "type");
            physical.tag = constructor_index as u32 + 1;
            physical.family_size = family.constructors.len() as u32;
            physical.host_id = crate::DataConId(model.inventory.len() as u64 + 1);
            let constructor = ConstructorId(model.inventory.len() as u32);
            let template = model.node(TypeNode::ConstructorTemplate {
                constructor,
                identity: physical.identity.clone(),
            });
            model.inventory.push(physical);
            model.edge(
                declaration,
                template,
                TypeEdge::Constructor(constructor_index as u32 + 1),
            );
            for (ordinal, &field) in fields.iter().enumerate() {
                let bound = bounds[field as usize % bounds.len()];
                let expression = match field % 5 {
                    0 => bound,
                    1 => {
                        let target = field as usize / 5 % families.len();
                        let application = model.node(TypeNode::NominalApplication);
                        model.edge(application, declarations[target], TypeEdge::Head);
                        for i in 0..families[target].parameters.len() {
                            model.edge(
                                application,
                                bounds[i % bounds.len()],
                                TypeEdge::Argument(i as u32),
                            );
                        }
                        application
                    }
                    2 => {
                        let function = model.node(TypeNode::Function(FunctionFlag::TypeToType));
                        model.edge(function, kind, TypeEdge::Multiplicity);
                        model.edge(function, bound, TypeEdge::Domain);
                        model.edge(function, bound, TypeEdge::Codomain);
                        function
                    }
                    3 => {
                        let application = model.node(TypeNode::Application);
                        model.edge(application, bound, TypeEdge::Function);
                        model.edge(application, bound, TypeEdge::ApplyArgument);
                        application
                    }
                    _ => {
                        let forall = model.node(TypeNode::ForAll(ForAllFlag::Specified));
                        model.edge(forall, kind, TypeEdge::Kind);
                        model.edge(forall, bound, TypeEdge::Body);
                        forall
                    }
                };
                model.edge(
                    template,
                    expression,
                    TypeEdge::Field {
                        ordinal: ordinal as u32,
                        source_rep: reps[ordinal],
                    },
                );
            }
        }
    }
    model.root = model.node(TypeNode::Root {
        domain: RootDomain::Closed,
        binders: Vec::new(),
        rendered: "generated closed data application".into(),
    });
    let body = model.node(TypeNode::NominalApplication);
    model.edge(model.root, body, TypeEdge::Body);
    model.edge(body, declarations[0], TypeEdge::Head);
    for i in 0..families[0].parameters.len() {
        let literal = model.node(TypeNode::Literal(TypeLiteral::Natural(i.to_string())));
        if i == 0 {
            model.argument = literal;
        }
        model.edge(body, literal, TypeEdge::Argument(i as u32));
    }
    model
}

fn permute<T>(values: &mut [T], mut seed: u64) {
    for i in (1..values.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        values.swap(i, (seed as usize) % (i + 1));
    }
}

fn remap(model: &Model, seed: u64) -> Model {
    let mut order: Vec<_> = (0..model.nodes.len()).collect();
    permute(&mut order, seed);
    let mut positions = vec![0; order.len()];
    for (new, &old) in order.iter().enumerate() {
        positions[old] = new;
    }
    let mut changed = model.clone();
    changed.nodes = order
        .iter()
        .map(|&i| {
            let mut node = model.nodes[i].clone();
            if let TypeNode::ConstructorTemplate { constructor, .. } = &mut node {
                constructor.0 = (model.inventory.len() - 1 - constructor.0 as usize) as u32;
            }
            if let TypeNode::Root { rendered, .. } = &mut node {
                rendered.push_str(" diagnostic");
            }
            node
        })
        .collect();
    changed.edges = model
        .edges
        .iter()
        .map(|&(s, t, r)| (positions[s], positions[t], r))
        .collect();
    permute(&mut changed.edges, seed.rotate_left(17));
    changed.inventory.reverse();
    changed.root = positions[model.root];
    changed.declaration = positions[model.declaration];
    changed.argument = positions[model.argument];
    changed
}

fn copy_expressions(model: &Model) -> Model {
    fn copy(model: &Model, result: &mut Model, map: &[usize], node: usize) -> usize {
        if !model.nodes[node].is_expression() {
            return map[node];
        }
        let new = result.node(model.nodes[node].clone());
        for &(source, target, role) in &model.edges {
            if source == node {
                let target = copy(model, result, map, target);
                result.edge(new, target, role);
            }
        }
        new
    }
    let mut result = model.clone();
    result.nodes.clear();
    result.edges.clear();
    let mut map = vec![usize::MAX; model.nodes.len()];
    for (i, node) in model.nodes.iter().enumerate() {
        if !node.is_expression() {
            map[i] = result.node(node.clone());
        }
    }
    for &(source, target, role) in &model.edges {
        if !model.nodes[source].is_expression() {
            let target = copy(model, &mut result, &map, target);
            result.edge(map[source], target, role);
        }
    }
    result.root = map[model.root];
    result.declaration = map[model.declaration];
    // This variant is only an identity comparison target, not a mutation input.
    result.argument = usize::MAX;
    result
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, max_shrink_iters: 4096, ..ProptestConfig::default() })]

    #[test]
    fn rooted_and_declaration_identity_match_independent_relation(
        families in cases(), other_families in cases(), seed in any::<u64>(), change in 0_u8..4,
    ) {
        let first = build(&families);
        let mut second = if change == 3 { build(&other_families) } else { first.clone() };
        if change == 1 {
            if let TypeNode::Declaration { restriction, .. } = &mut second.nodes[first.declaration] {
                *restriction = SyntaxRestriction::EffectHead;
            }
        }
        if change == 2 {
            second.nodes[first.argument] = TypeNode::Literal(TypeLiteral::Natural("99".into()));
        }
        second = remap(&second, seed);
        second.node(TypeNode::Literal(TypeLiteral::Symbol("disconnected".into())));
        let expected = relation(&first, &second);
        prop_assert_eq!(expected[first.root][second.root], change == 0 || (change == 3 && expected[first.root][second.root]));
        if change <= 2 { prop_assert_eq!(expected[first.declaration][second.declaration], change != 1); }
        let a = first.publish();
        let b = second.publish();
        let root_equal = a.rooted_identity_eq(TypeNodeId::new(first.root), &b, TypeNodeId::new(second.root), &mut TypeWorkBudget::new(1_000_000))?;
        prop_assert_eq!(root_equal, expected[first.root][second.root]);
        let declaration_equal = a.declaration_identity_eq(TypeNodeId::new(first.declaration), &b, TypeNodeId::new(second.declaration), &mut TypeWorkBudget::new(1_000_000))?;
        prop_assert_eq!(declaration_equal, expected[first.declaration][second.declaration]);
        prop_assert_eq!(b.rooted_identity_eq(TypeNodeId::new(second.root), &a, TypeNodeId::new(first.root), &mut TypeWorkBudget::new(1_000_000))?, root_equal);
        prop_assert_eq!(b.declaration_identity_eq(TypeNodeId::new(second.declaration), &a, TypeNodeId::new(first.declaration), &mut TypeWorkBudget::new(1_000_000))?, declaration_equal);
        // Erasing sharing copies each expression occurrence but retains nominal
        // recursion. The relation and implementation must both accept it.
        let copied = copy_expressions(&first);
        let expected_copy = relation(&first, &copied);
        prop_assert!(expected_copy[first.root][copied.root]);
        prop_assert!(expected_copy[first.declaration][copied.declaration]);
        let c = copied.publish();
        prop_assert!(a.rooted_identity_eq(TypeNodeId::new(first.root), &c, TypeNodeId::new(copied.root), &mut TypeWorkBudget::new(1_000_000))?);
        prop_assert!(a.declaration_identity_eq(TypeNodeId::new(first.declaration), &c, TypeNodeId::new(copied.declaration), &mut TypeWorkBudget::new(1_000_000))?);
    }
}
