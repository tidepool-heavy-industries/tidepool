//! Bounded representation-only graph properties. The oracle uses an edge list
//! and relation refinement, never the production cursor or identity walk.

use super::*;
use proptest::prelude::*;
use std::sync::Arc;

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
    roots: Vec<usize>,
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
        roots: Vec::new(),
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
    model.roots.push(model.root);
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
    changed.roots = model.roots.iter().map(|&root| positions[root]).collect();
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
    result.roots = model.roots.iter().map(|&root| map[root]).collect();
    result.declaration = map[model.declaration];
    // This variant is only an identity comparison target, not a mutation input.
    result.argument = usize::MAX;
    result
}

fn property_config() -> ProptestConfig {
    use proptest::test_runner::FileFailurePersistence;

    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

proptest! {
    #![proptest_config(property_config())]

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
        if change <= 2 { prop_assert_eq!(expected[first.root][second.root], change == 0); }
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

    #[test]
    fn shared_body_roots_match_relation_and_cursor_instantiation(
        families in cases(), seed in any::<u64>(),
    ) {
        let mut families = families;
        if families[0].constructors[0].is_empty() {
            families[0].constructors[0].push(0);
        }
        let mut model = build(&families);
        let Some(body) = model.edges.iter().find_map(|&(source, target, role)| {
            (source == model.root && role == TypeEdge::Body).then_some(target)
        }) else {
            return Err(TestCaseError::fail("constructed root must have one body"));
        };
        let second_root = model.node(TypeNode::Root {
            domain: RootDomain::Closed,
            binders: Vec::new(),
            rendered: "second diagnostic label for shared body".into(),
        });
        model.edge(second_root, body, TypeEdge::Body);
        model.roots.push(second_root);

        let reordered = remap(&model, seed);
        let expected = relation(&model, &reordered);
        let first = Arc::new(model.publish());
        let second = Arc::new(reordered.publish());
        for &first_root in &model.roots {
            for &second_root in &reordered.roots {
                let expected_equal = expected[first_root][second_root];
                prop_assert!(expected_equal, "shared-body roots differ only in diagnostics and node layout");
                prop_assert_eq!(
                    first.rooted_identity_eq(
                        TypeNodeId::new(first_root),
                        &second,
                        TypeNodeId::new(second_root),
                        &mut TypeWorkBudget::new(1_000_000),
                    )?,
                    expected_equal,
                );
                prop_assert_eq!(
                    first.rooted_compatible(
                        crate::execution_schema::TypeNodeId(first_root as u32),
                        &second,
                        crate::execution_schema::TypeNodeId(second_root as u32),
                        &mut TypeWorkBudget::new(1_000_000),
                    )?,
                    expected_equal,
                );
            }
        }

        let primary = first.open_root(
            crate::execution_schema::TypeNodeId(model.roots[0] as u32),
            &mut TypeWorkBudget::new(1_000_000),
        )?;
        let sibling = first.open_root(
            crate::execution_schema::TypeNodeId(model.roots[1] as u32),
            &mut TypeWorkBudget::new(1_000_000),
        )?;
        prop_assert_eq!(primary.expression().index(), body);
        prop_assert_eq!(sibling.expression().index(), body);
        prop_assert_eq!(primary.rendered(), "generated closed data application");
        prop_assert_eq!(sibling.rendered(), "second diagnostic label for shared body");
        prop_assert!(Arc::ptr_eq(primary.owner(), sibling.owner()));
        let TypeView::Data(primary_data) = primary.view(&mut TypeWorkBudget::new(1_000_000))? else {
            return Err(TestCaseError::fail("shared saturated body must yield a data view"));
        };
        let TypeView::Data(sibling_data) = sibling.view(&mut TypeWorkBudget::new(1_000_000))? else {
            return Err(TestCaseError::fail("shared saturated body must yield a data view"));
        };
        prop_assert_eq!(primary_data.family(), sibling_data.family());
        prop_assert_eq!(primary_data.argument_count(), sibling_data.argument_count());
        let constructor = primary_data.constructors().next().unwrap();
        let primary_fields = primary_data.fields(constructor, &mut TypeWorkBudget::new(1_000_000))?.unwrap();
        let sibling_fields = sibling_data.fields(constructor, &mut TypeWorkBudget::new(1_000_000))?.unwrap();
        prop_assert!(!primary_fields.is_empty(), "targeted constructor must exercise a field closure");
        prop_assert_eq!(primary_fields.len(), sibling_fields.len());
        for (primary_field, sibling_field) in primary_fields.iter().zip(&sibling_fields) {
            prop_assert_eq!(primary_field.expression(), sibling_field.expression());
            prop_assert!(Arc::ptr_eq(primary_field.owner(), sibling_field.owner()));
            prop_assert_eq!(
                view_class(&primary_field.view(&mut TypeWorkBudget::new(1_000_000))?),
                view_class(&sibling_field.view(&mut TypeWorkBudget::new(1_000_000))?),
            );
        }

        let mut invalid = model.clone();
        invalid.edge(second_root, body, TypeEdge::Body);
        prop_assert_eq!(
            TypeGraph::validate(invalid.storage(), &invalid.inventory, GraphLimits::default()),
            Err(TypeGraphError::InvalidCardinality(second_root)),
        );
    }

    #[test]
    fn publication_orders_edges_and_preserves_content_evidence_contract(
        families in cases(), seed in any::<u64>(),
    ) {
        let mut model = build(&families);
        let opaque = model.node(TypeNode::Declaration {
            identity: tests::identity("DisconnectedOpaque", "type"),
            parameters: Vec::new(),
            form: DeclarationForm::Opaque { head_kind: NominalHeadKind::Family, reason: "diagnostic".into() },
            restriction: SyntaxRestriction::None,
        });
        let (first, work) = TypeGraph::validate_with_work(model.storage(), &model.inventory, GraphLimits::default())?;
        let expected = model.adjacency();
        for (node, edges) in expected.iter().enumerate() {
            let actual: Vec<_> = first.ordered_edges(TypeNodeId::new(node))
                .map(|edge| (*edge.weight(), edge.target().index())).collect();
            prop_assert_eq!(&actual, edges);
        }
        let mut reordered = model.clone();
        permute(&mut reordered.edges, seed);
        let second = reordered.publish();
        prop_assert!(first.content_eq(&second));
        prop_assert!(first.evidence_eq(&second));
        prop_assert_eq!(commitment(&first, false), commitment(&second, false));
        prop_assert_eq!(commitment(&first, true), commitment(&second, true));
        // The reported validation cost includes canonicalization. Its exact
        // boundary must admit the same input and refuse one work unit less.
        let exact_limits = GraphLimits { max_work: work, ..GraphLimits::default() };
        prop_assert!(TypeGraph::validate(model.storage(), &model.inventory, exact_limits).is_ok());
        prop_assert_eq!(TypeGraph::validate(model.storage(), &model.inventory, GraphLimits { max_work: work - 1, ..GraphLimits::default() }), Err(TypeGraphError::Limit("work")));
        if let TypeNode::Root { rendered, .. } = &mut reordered.nodes[model.root] { rendered.push_str(" changed"); }
        if let TypeNode::Declaration { form: DeclarationForm::Opaque { reason, .. }, .. } = &mut reordered.nodes[opaque] { reason.push_str(" changed"); }
        let diagnostics = reordered.publish();
        prop_assert!(!first.content_eq(&diagnostics));
        prop_assert!(first.evidence_eq(&diagnostics));
        prop_assert_ne!(commitment(&first, false), commitment(&diagnostics, false));
        prop_assert_eq!(commitment(&first, true), commitment(&diagnostics, true));
        prop_assert!(first.rooted_identity_eq(TypeNodeId::new(model.root), &diagnostics, TypeNodeId::new(model.root), &mut TypeWorkBudget::new(1_000_000))?);
        let mut budget = TypeWorkBudget::new(1_000_000);
        prop_assert!(first.rooted_identity_eq(TypeNodeId::new(model.root), &first, TypeNodeId::new(model.root), &mut budget)?);
        let spent = budget.spent();
        prop_assert!(first.rooted_identity_eq(TypeNodeId::new(model.root), &first, TypeNodeId::new(model.root), &mut TypeWorkBudget::new(spent))?);
        prop_assert_eq!(first.rooted_identity_eq(TypeNodeId::new(model.root), &first, TypeNodeId::new(model.root), &mut TypeWorkBudget::new(spent - 1)), Err(TypeGraphError::TraversalWork));
    }

    #[test]
    fn constructor_pairing_refuses_single_inventory_defects(
        families in cases(), selected in any::<u16>(), defect in 0_u8..7,
    ) {
        let model = build(&families);
        let graph = model.publish();
        let original = model.inventory.clone();
        let mut changed = original.clone();
        let physical = &mut changed[selected as usize % original.len()];
        match defect {
            0 => physical.identity.occurrence.push_str("Wrong"),
            1 => physical.family.occurrence.push_str("Wrong"),
            2 => physical.result_rep = RuntimeRep::Void,
            3 => physical.tag = 0,
            4 => physical.family_size += 1,
            5 => physical.field_reps.push(RuntimeRep::LiftedRef),
            _ => {
                if let Some(rep) = physical.field_reps.first_mut() {
                    *rep = if *rep == RuntimeRep::Int(64) { RuntimeRep::LiftedRef } else { RuntimeRep::Int(64) };
                } else {
                    physical.field_reps.push(RuntimeRep::LiftedRef);
                }
            }
        }
        prop_assert!(matches!(graph.check_constructor_pairing(&changed), Err(TypeGraphError::InvalidConstructor(_))));
        prop_assert!(matches!(TypeGraph::validate(model.storage(), &changed, GraphLimits::default()), Err(TypeGraphError::InvalidConstructor(_))));
        // Refusal and repeated inventory checks leave the frozen owner intact.
        prop_assert_eq!(graph.check_constructor_pairing(&original), Ok(()));
        let work = graph.check_constructor_pairing_with_work(&original, GraphLimits::default())?;
        prop_assert_eq!(graph.check_constructor_pairing_with_work(&original, GraphLimits { max_work: work, ..GraphLimits::default() })?, work);
        prop_assert_eq!(graph.check_constructor_pairing_with_work(&original, GraphLimits { max_work: work - 1, ..GraphLimits::default() }), Err(TypeGraphError::Limit("work")));
    }

    #[test]
    fn single_expression_or_scope_defects_refuse(families in cases(), defect in 0_u8..4) {
        let mut model = build(&families);
        let expected = match defect {
            0 => {
                model.publish();
                model.edge(model.root, model.argument, TypeEdge::Body);
                TypeGraphError::InvalidCardinality(model.root)
            }
            1 => {
                model.publish();
                model.edges.retain(|&(source, _, role)| source != model.root || role != TypeEdge::Body);
                TypeGraphError::InvalidCardinality(model.root)
            }
            2 => {
                let application = model.node(TypeNode::Application);
                model.edge(application, model.argument, TypeEdge::Function);
                model.edge(application, model.argument, TypeEdge::ApplyArgument);
                model.publish();
                let edge = model.edges.iter_mut().find(|(source, _, role)| *source == application && *role == TypeEdge::ApplyArgument).unwrap();
                edge.1 = application;
                TypeGraphError::ExpressionCycle
            }
            _ => {
                let bound = model.node(TypeNode::Bound(0));
                let kind = model.node(TypeNode::Literal(TypeLiteral::Symbol("kind".into())));
                let open = model.node(TypeNode::Root {
                    domain: RootDomain::ConstructorScheme,
                    binders: vec![SourceBinderFlag::Specified],
                    rendered: "open bound".into(),
                });
                model.edge(open, kind, TypeEdge::BinderKind(0));
                model.edge(open, bound, TypeEdge::Body);
                model.publish();
                // The same bound expression was valid under the open root;
                // sharing it under the closed root must independently refuse.
                let edge = model.edges.iter_mut().find(|(source, _, role)| *source == model.root && *role == TypeEdge::Body).unwrap();
                edge.1 = bound;
                TypeGraphError::InvalidScope(model.root)
            }
        };
        prop_assert_eq!(TypeGraph::validate(model.storage(), &model.inventory, GraphLimits::default()), Err(expected));
    }

    #[test]
    fn cursor_clone_read_and_augmentation_retain_frozen_graph(families in cases()) {
        let model = build(&families);
        let owner = Arc::new(model.publish());
        let before = commitment(&owner, false);
        let root = crate::execution_schema::TypeNodeId(model.root as u32);
        let cursor = owner.open_root(root, &mut TypeWorkBudget::new(1_000_000))?;
        let cloned = cursor.clone();
        prop_assert!(Arc::ptr_eq(cursor.owner(), cloned.owner()));
        prop_assert_eq!(cursor.expression(), cloned.expression());
        prop_assert_eq!(cursor.rendered(), cloned.rendered());
        let TypeView::Data(data) = cloned.view(&mut TypeWorkBudget::new(1_000_000))? else {
            return Err(TestCaseError::fail("saturated data root must yield a data view"));
        };
        prop_assert_eq!(data.family(), &model.inventory[0].family);
        prop_assert_eq!(data.argument_count(), families[0].parameters.len());
        let expected_ids: Vec<_> = (0..families[0].constructors.len()).map(|i| ConstructorId(i as u32)).collect();
        prop_assert_eq!(data.constructors().collect::<Vec<_>>(), expected_ids);
        for constructor in data.constructors() {
            let fields = data.fields(constructor, &mut TypeWorkBudget::new(1_000_000))?.unwrap();
            let template = model.nodes.iter().position(|node| matches!(node, TypeNode::ConstructorTemplate { constructor: id, .. } if *id == constructor)).unwrap();
            let expected = &model.adjacency()[template];
            prop_assert_eq!(fields.len(), expected.len());
            for (field, &(_, target)) in fields.iter().zip(expected) {
                prop_assert_eq!(field.expression().index(), target);
                prop_assert!(Arc::ptr_eq(field.owner(), &owner));
                let field_clone = field.clone();
                let view = field_clone.view(&mut TypeWorkBudget::new(1_000_000))?;
                let actual = view_class(&view);
                let expected = match model.nodes[target] {
                    TypeNode::NominalApplication => 0,
                    TypeNode::Function(_) => 1,
                    TypeNode::ForAll(_) => 2,
                    // Bound arguments resolve to root literals; applying a
                    // literal also remains unnormalized, not constructible.
                    TypeNode::Bound(_) | TypeNode::Application => 3,
                    _ => unreachable!("generator field shape"),
                };
                prop_assert_eq!(actual, expected);
                if let TypeView::Data(nested) = view {
                    // One more generation exercises composed environments and
                    // regular/nonregular recursion without expanding a tree.
                    for nested_constructor in nested.constructors() {
                        let nested_fields = nested.fields(nested_constructor, &mut TypeWorkBudget::new(1_000_000))?.unwrap();
                        prop_assert_eq!(nested_fields.len(), model.inventory[nested_constructor.0 as usize].field_reps.len());
                        for nested_field in nested_fields {
                            let _ = nested_field.view(&mut TypeWorkBudget::new(1_000_000))?;
                            prop_assert!(Arc::ptr_eq(nested_field.owner(), &owner));
                        }
                    }
                }
            }
        }
        prop_assert!(data.fields(ConstructorId(model.inventory.len() as u32), &mut TypeWorkBudget::new(1_000_000))?.is_none());
        prop_assert_eq!(commitment(&owner, false), before);
        let mut augmented = model.clone();
        augmented.node(TypeNode::Literal(TypeLiteral::Symbol("later disconnected expression".into())));
        let new_owner = Arc::new(augmented.publish());
        prop_assert_eq!(owner.graph().node_count(), model.nodes.len());
        prop_assert_eq!(new_owner.graph().node_count(), model.nodes.len() + 1);
        prop_assert!(owner.rooted_identity_eq(TypeNodeId::new(model.root), &new_owner, TypeNodeId::new(model.root), &mut TypeWorkBudget::new(1_000_000))?);
        prop_assert!(matches!(cursor.view(&mut TypeWorkBudget::new(1_000_000))?, TypeView::Data(_)));
    }
}

fn commitment(graph: &TypeGraph, evidence: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if evidence {
        graph.write_evidence(|part| bytes.extend_from_slice(part));
    } else {
        graph.write_content(|part| bytes.extend_from_slice(part));
    }
    bytes
}

fn view_class(view: &TypeView) -> u8 {
    match view {
        TypeView::Data(_) => 0,
        TypeView::Unconstructible(ConstructionRefusal::Function) => 1,
        TypeView::Unconstructible(ConstructionRefusal::Polymorphic) => 2,
        TypeView::Unconstructible(ConstructionRefusal::Unnormalized) => 3,
        _ => 4,
    }
}

#[test]
fn generated_topology_distribution_and_shrinking_are_observable() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{RngAlgorithm, TestRng, TestRunner};

    let mut runner = TestRunner::new_with_rng(
        ProptestConfig {
            failure_persistence: None,
            ..ProptestConfig::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[42; 32]),
    );
    let strategy = (cases(), cases(), 0_u8..4);
    let mut family_counts = [0_usize; 4];
    let mut field_shapes = [0_usize; 5];
    let mut field_counts = [0_usize; 4];
    let mut parameter_flags = [0_usize; 4];
    let mut reps = [0_usize; 3];
    let mut cyclic = 0;
    let mut diamonds = 0;
    let mut disconnected = 0;
    let mut copied_larger = 0;
    let mut largest = 0;
    let mut identity_modes = [0_usize; 4];
    let mut root_outcomes = [0_usize; 2];
    let mut declaration_outcomes = [0_usize; 2];
    for _ in 0..256 {
        let (families, other_families, mode) = strategy.new_tree(&mut runner).unwrap().current();
        identity_modes[mode as usize] += 1;
        family_counts[families.len()] += 1;
        for family in &families {
            for &parameter in &family.parameters {
                parameter_flags[parameter as usize] += 1;
            }
            for fields in &family.constructors {
                field_counts[fields.len()] += 1;
                for &field in fields {
                    field_shapes[field as usize % 5] += 1;
                    reps[field as usize % 3] += 1;
                }
            }
        }
        let model = build(&families);
        let mut other = if mode == 3 {
            build(&other_families)
        } else {
            model.clone()
        };
        match mode {
            1 => {
                if let TypeNode::Declaration { restriction, .. } =
                    &mut other.nodes[model.declaration]
                {
                    *restriction = SyntaxRestriction::EffectHead;
                }
            }
            2 => other.nodes[model.argument] = TypeNode::Literal(TypeLiteral::Natural("99".into())),
            _ => {}
        }
        let related = relation(&model, &other);
        root_outcomes[usize::from(related[model.root][other.root])] += 1;
        declaration_outcomes[usize::from(related[model.declaration][other.declaration])] += 1;
        largest = largest.max(model.nodes.len());
        let edges = model.adjacency();
        diamonds += usize::from(edges.iter().any(|edges| {
            edges
                .iter()
                .enumerate()
                .any(|(i, (_, target))| edges[i + 1..].iter().any(|(_, other)| target == other))
        }));
        // Finite edge-list reachability detects cycles through declarations;
        // expression-only cycles are independently rejected by publication.
        let mut reachable = vec![vec![false; model.nodes.len()]; model.nodes.len()];
        for &(source, target, _) in &model.edges {
            reachable[source][target] = true;
        }
        for middle in 0..model.nodes.len() {
            for source in 0..model.nodes.len() {
                for target in 0..model.nodes.len() {
                    let via_middle = reachable[source][middle] && reachable[middle][target];
                    reachable[source][target] |= via_middle;
                }
            }
        }
        cyclic += usize::from((0..model.nodes.len()).any(|i| reachable[i][i]));
        disconnected += usize::from(
            (0..model.nodes.len()).any(|i| i != model.root && !reachable[model.root][i]),
        );
        copied_larger += usize::from(copy_expressions(&model).nodes.len() > model.nodes.len());
    }
    let mut shrinking = cases().new_tree(&mut runner).unwrap();
    let mut shrink_steps = 0;
    while shrinking.simplify() {
        shrink_steps += 1;
        assert!(
            shrink_steps < 4096,
            "bounded configuration must finish shrinking"
        );
    }
    let minimal = shrinking.current();
    eprintln!("type graph coverage: samples=256 families={family_counts:?} field_shapes={field_shapes:?} field_counts={field_counts:?} parameter_flags={parameter_flags:?} reps={reps:?} identity_modes={identity_modes:?} root_outcomes_false_true={root_outcomes:?} declaration_outcomes_false_true={declaration_outcomes:?} cycles={cyclic} diamonds={diamonds} disconnected={disconnected} copied_larger={copied_larger} max_nodes={largest} shrink_steps={shrink_steps} minimal={minimal:?}");
    assert!(family_counts[1..].iter().all(|&count| count >= 16));
    assert!(field_shapes.iter().all(|&count| count >= 32));
    assert!(field_counts.iter().all(|&count| count >= 32));
    assert!(parameter_flags.iter().all(|&count| count >= 32));
    assert!(reps.iter().all(|&count| count >= 32));
    assert!(identity_modes.iter().all(|&count| count >= 16));
    assert!(root_outcomes.iter().all(|&count| count >= 16));
    assert!(declaration_outcomes.iter().all(|&count| count >= 16));
    assert!(cyclic >= 16 && diamonds >= 64 && disconnected >= 32 && copied_larger >= 64);
    assert!(shrink_steps > 0);
    assert_eq!(minimal.len(), 1);
    assert_eq!(minimal[0].parameters, vec![0]);
    assert_eq!(minimal[0].constructors, vec![Vec::<u8>::new()]);
}
