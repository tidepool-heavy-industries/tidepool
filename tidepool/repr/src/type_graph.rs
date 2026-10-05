//! Finite compiler type declarations and scoped expressions.
//!
//! Node weights retain metadata; all child relationships live in the graph.
//! Original compiler admission and physical constructor authority remain with
//! their existing owners. A graph describes type evidence, not authority.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use petgraph::graph::{Edges, Graph, NodeIndex};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::Directed;

use crate::execution_schema::{ConstructorDecl, ConstructorId, DecodeLimits, RuntimeRep, SymbolIdentity};

pub type TypeNodeId = NodeIndex<u32>;
pub type GraphStorage = Graph<TypeNode, TypeEdge, Directed, u32>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RootDomain {
    Closed,
    ConstructorScheme,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceBinderFlag {
    Specified,
    Inferred,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ParameterFlag {
    NamedRequired,
    NamedSpecified,
    NamedInferred,
    AnonymousVisible,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ForAllFlag {
    Required,
    Specified,
    Inferred,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionFlag {
    TypeToType,
    TypeToConstraint,
    ConstraintToType,
    ConstraintToConstraint,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NominalHeadKind {
    Constructor,
    Family,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SyntaxRestriction {
    None,
    EffectHead,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DeclarationForm {
    Data,
    Newtype { eta_arity: u32 },
    Text,
    Integer,
    Natural,
    Scalar(RuntimeRep),
    Opaque { head_kind: NominalHeadKind, reason: String },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TypeLiteral {
    Natural(String),
    Symbol(String),
    Character(char),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TypeNode {
    Root {
        domain: RootDomain,
        binders: Vec<SourceBinderFlag>,
        rendered: String,
    },
    Declaration {
        identity: SymbolIdentity,
        parameters: Vec<ParameterFlag>,
        form: DeclarationForm,
        restriction: SyntaxRestriction,
    },
    ConstructorTemplate {
        constructor: ConstructorId,
        /// Derived from the physical inventory; the wire encodes only its ID.
        identity: SymbolIdentity,
    },
    Bound(u32),
    NominalApplication,
    Application,
    Function(FunctionFlag),
    ForAll(ForAllFlag),
    Literal(TypeLiteral),
}

impl TypeNode {
    pub fn is_expression(&self) -> bool {
        !matches!(self, Self::Root { .. } | Self::Declaration { .. } | Self::ConstructorTemplate { .. })
    }
}

/// Roles are ordered by their wire tag, then by their source ordinal. Parallel
/// edges with different ordinals are meaningful, including `Pair a a`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeEdge {
    BinderKind(u32),
    Body,
    Head,
    Argument(u32),
    Function,
    ApplyArgument,
    Multiplicity,
    Domain,
    Codomain,
    Kind,
    Constructor(u32),
    Field { ordinal: u32, source_rep: RuntimeRep },
    AliasRhs,
}

/// The sole immutable semantic graph. Artifacts share this owner with `Arc`.
/// Construction and validation are private to the publication entry point.
#[derive(Debug)]
pub struct TypeGraph {
    graph: GraphStorage,
}

impl TypeGraph {
    /// Publish a complete graph once, using the enclosing decoder's remaining
    /// budgets. The physical inventory is authenticated by its existing owner.
    pub fn validate(
        graph: GraphStorage,
        constructors: &[ConstructorDecl],
        limits: GraphLimits,
    ) -> Result<Self, TypeGraphError> {
        Self::validate_with_work(graph, constructors, limits).map(|(graph, _)| graph)
    }

    pub fn validate_with_work(
        mut graph: GraphStorage,
        constructors: &[ConstructorDecl],
        limits: GraphLimits,
    ) -> Result<(Self, usize), TypeGraphError> {
        let work = validate_graph(&graph, constructors, limits)?;
        // petgraph's adjacency lists reverse insertion order. Canonicalize
        // once, preserving node indices and repeated targets at distinct slots.
        let mut edges: Vec<_> = graph.edge_references()
            .map(|edge| (edge.source(), edge.target(), *edge.weight())).collect();
        edges.sort_unstable_by(|first, second| first.0.index().cmp(&second.0.index())
            .then_with(|| second.2.cmp(&first.2)));
        graph.clear_edges();
        for (source, target, role) in edges {
            graph.add_edge(source, target, role);
        }
        Ok((Self { graph }, work))
    }

    pub fn graph(&self) -> &GraphStorage {
        &self.graph
    }

    /// Borrowed ascending role/ordinal iteration. Publication fixes insertion
    /// order once; reads neither sort nor create an adjacency representation.
    pub fn ordered_edges(&self, node: TypeNodeId) -> Edges<'_, TypeEdge, Directed, u32> {
        self.graph.edges(node)
    }

    /// Recheck only the physical inventory pairing after a caller changes its
    /// enclosing unpublished program. The already frozen expression graph is
    /// not reparsed or revalidated under a different scope.
    pub fn check_constructor_pairing(&self, constructors: &[ConstructorDecl]) -> Result<(), TypeGraphError> {
        self.check_constructor_pairing_with_work(constructors, GraphLimits::default()).map(|_| ())
    }

    pub fn check_constructor_pairing_with_work(&self, constructors: &[ConstructorDecl], limits: GraphLimits)
        -> Result<usize, TypeGraphError>
    {
        let mut budget = Budget { bytes: 0, work: 0, limits };
        for node in self.graph.node_indices() {
            budget.work(1)?;
            match &self.graph[node] {
                TypeNode::ConstructorTemplate { constructor, identity } => {
                    let physical = constructors.get(constructor.0 as usize)
                        .ok_or(TypeGraphError::InvalidConstructor(node.index()))?;
                    if identity != &physical.identity || physical.result_rep != RuntimeRep::LiftedRef
                        || physical.field_reps.len() != self.graph.edges(node).count() {
                        return Err(TypeGraphError::InvalidConstructor(node.index()));
                    }
                    let parent = self.graph.edges_directed(node, petgraph::Incoming).next()
                        .ok_or(TypeGraphError::InvalidConstructor(node.index()))?;
                    match (&self.graph[parent.source()], parent.weight()) {
                        (TypeNode::Declaration { identity, .. }, TypeEdge::Constructor(tag))
                            if identity == &physical.family && *tag == physical.tag => {}
                        _ => return Err(TypeGraphError::InvalidConstructor(node.index())),
                    }
                    for edge in self.ordered_edges(node) {
                        budget.work(1)?;
                        match edge.weight() {
                            TypeEdge::Field { ordinal, source_rep }
                                if physical.field_reps.get(*ordinal as usize) == Some(source_rep) => {}
                            _ => return Err(TypeGraphError::InvalidConstructor(node.index())),
                        }
                    }
                }
                TypeNode::Declaration { form: DeclarationForm::Data, .. } => {
                    let count = self.graph.edges(node).filter(|edge|
                        matches!(edge.weight(), TypeEdge::Constructor(_))).count();
                    for edge in self.ordered_edges(node) {
                        budget.work(1)?;
                        if matches!(edge.weight(), TypeEdge::Constructor(_)) {
                            let TypeNode::ConstructorTemplate { constructor, .. } = &self.graph[edge.target()] else {
                                return Err(TypeGraphError::InvalidConstructor(node.index()));
                            };
                            if constructors.get(constructor.0 as usize).is_none_or(|physical|
                                physical.family_size as usize != count) {
                                return Err(TypeGraphError::InvalidConstructor(node.index()));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(budget.work)
    }

    pub fn content_eq(&self, other: &Self) -> bool { self.ordered_eq(other, false) }

    /// Whole ordered graph identity, excluding only diagnostic renderings and
    /// opaque reasons. Root, node and edge positions remain part of identity.
    pub fn evidence_eq(&self, other: &Self) -> bool { self.ordered_eq(other, true) }

    fn ordered_eq(&self, other: &Self, evidence: bool) -> bool {
        self.graph.node_count() == other.graph.node_count()
            && self.graph.edge_count() == other.graph.edge_count()
            && self.graph.node_indices().all(|node| {
                let metadata = if evidence { node_evidence_equal(&self.graph[node], &other.graph[node], false) }
                    else { self.graph[node] == other.graph[node] };
                metadata && self.ordered_edges(node).map(|edge| (edge.target(), *edge.weight()))
                    .eq(other.ordered_edges(node).map(|edge| (edge.target(), *edge.weight())))
            })
    }

    /// Stable explicit framing for the ordered graph's evidence commitment.
    /// The enclosing request owner adds its constructor inventory, endpoints
    /// and authenticated context to its existing commitment domain.
    pub fn write_evidence(&self, sink: impl FnMut(&[u8])) { self.write_identity(sink, true); }

    pub fn write_content(&self, sink: impl FnMut(&[u8])) { self.write_identity(sink, false); }

    fn write_identity(&self, mut sink: impl FnMut(&[u8]), evidence: bool) {
        let mut frame = |bytes: &[u8]| { sink(&(bytes.len() as u64).to_le_bytes()); sink(bytes); };
        frame(if evidence { b"Tidepool.TypeGraph.evidence.v1" } else { b"Tidepool.TypeGraph.content.v1" });
        frame(&(self.graph.node_count() as u64).to_le_bytes());
        frame(&(self.graph.edge_count() as u64).to_le_bytes());
        for node in self.graph.node_indices() {
            write_node(&self.graph[node], evidence, &mut frame);
            frame(&(self.graph.edges(node).count() as u64).to_le_bytes());
            for edge in self.ordered_edges(node) {
                frame(&(edge.target().index() as u32).to_le_bytes());
                let (tag, index) = edge.weight().slot();
                frame(&[tag]); frame(&index.to_le_bytes());
                if let TypeEdge::Field { source_rep, .. } = edge.weight() {
                    write_rep(*source_rep, &mut frame);
                }
            }
        }
    }
}

impl Default for TypeGraph {
    fn default() -> Self { Self { graph: GraphStorage::new() } }
}

impl PartialEq for TypeGraph {
    fn eq(&self, other: &Self) -> bool { self.content_eq(other) }
}
impl Eq for TypeGraph {}
impl Hash for TypeGraph {
    fn hash<H: Hasher>(&self, state: &mut H) { self.write_content(|bytes| state.write(bytes)); }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphLimits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_bytes: usize,
    pub max_work: usize,
}

impl Default for GraphLimits {
    fn default() -> Self {
        DecodeLimits::default().into()
    }
}

impl From<DecodeLimits> for GraphLimits {
    fn from(limits: DecodeLimits) -> Self {
        Self { max_nodes: limits.max_type_nodes, max_edges: limits.max_work,
            max_bytes: limits.max_bytes, max_work: limits.max_work }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TypeGraphError {
    #[error("finite type graph exceeds {0} budget")]
    Limit(&'static str),
    #[error("finite type graph has an invalid reference at node {0}")]
    InvalidReference(usize),
    #[error("finite type graph has an invalid role at node {0}")]
    InvalidRole(usize),
    #[error("finite type graph has incomplete or repeated roles at node {0}")]
    InvalidCardinality(usize),
    #[error("finite type graph has an invalid nominal identity at node {0}")]
    InvalidIdentity(usize),
    #[error("finite type graph has an invalid literal at node {0}")]
    InvalidLiteral(usize),
    #[error("finite type graph has an invalid representation at node {0}")]
    InvalidRepresentation(usize),
    #[error("finite type graph has a conflicting constructor template at node {0}")]
    InvalidConstructor(usize),
    #[error("finite type graph repeats a nominal declaration at node {0}")]
    DuplicateDeclaration(usize),
    #[error("finite type graph has an expression cycle")]
    ExpressionCycle,
    #[error("finite type graph has an out-of-scope variable at node {0}")]
    InvalidScope(usize),
    #[error("finite type graph node {0} is not a root")]
    NotRoot(usize),
    #[error("finite type graph traversal exceeds its work budget")]
    TraversalWork,
}

/// One budget shared by normalization, syntax summaries and compatibility.
/// Callers retain it across selected fields and recursive operations.
#[derive(Debug)]
pub struct TypeWorkBudget { limit: usize, spent: usize }

impl TypeWorkBudget {
    pub fn new(limit: usize) -> Self { Self { limit, spent: 0 } }
    pub fn charge(&mut self, work: usize) -> Result<(), TypeGraphError> {
        let spent = self.spent.checked_add(work).ok_or(TypeGraphError::TraversalWork)?;
        if spent > self.limit { return Err(TypeGraphError::TraversalWork); }
        self.spent = spent;
        Ok(())
    }
    pub fn spent(&self) -> usize { self.spent }
    pub fn remaining(&self) -> usize { self.limit - self.spent }
}

impl TypeGraph {
    /// Finite exact scoped type identity, independent of storage indices and
    /// diagnostics. Declaration templates compare under their formal telescope;
    /// this walk never instantiates recursively growing constructor fields.
    pub fn rooted_identity_eq(&self, root: TypeNodeId, other: &Self, other_root: TypeNodeId,
        budget: &mut TypeWorkBudget) -> Result<bool, TypeGraphError>
    {
        for (graph, node) in [(self, root), (other, other_root)] {
            if !matches!(graph.graph.node_weight(node), Some(TypeNode::Root { .. })) {
                return Err(TypeGraphError::NotRoot(node.index()));
            }
        }
        self.node_identity_eq(root, other, other_root, budget)
    }

    /// Compare a declaration's complete formal schema, including parameter
    /// kinds and original constructor field templates. Matching only nominal
    /// family names and physical layouts is insufficient.
    pub fn declaration_identity_eq(&self, declaration: TypeNodeId, other: &Self,
        other_declaration: TypeNodeId, budget: &mut TypeWorkBudget) -> Result<bool, TypeGraphError>
    {
        for (graph, node) in [(self, declaration), (other, other_declaration)] {
            if !matches!(graph.graph.node_weight(node), Some(TypeNode::Declaration { .. })) {
                return Err(TypeGraphError::InvalidRole(node.index()));
            }
        }
        self.node_identity_eq(declaration, other, other_declaration, budget)
    }

    fn node_identity_eq(&self, first: TypeNodeId, other: &Self, second: TypeNodeId,
        budget: &mut TypeWorkBudget) -> Result<bool, TypeGraphError>
    {
        let mut pending = vec![(first, second)];
        let mut visited = BTreeSet::new();
        while let Some((first, second)) = pending.pop() {
            budget.charge(1)?;
            if !visited.insert((first.index(), second.index())) { continue; }
            let first_node = self.graph.node_weight(first)
                .ok_or(TypeGraphError::InvalidReference(first.index()))?;
            let second_node = other.graph.node_weight(second)
                .ok_or(TypeGraphError::InvalidReference(second.index()))?;
            budget.charge(evidence_metadata_work(first_node).max(evidence_metadata_work(second_node)))?;
            if !node_evidence_equal(first_node, second_node, true) { return Ok(false); }
            let mut first_edges = self.ordered_edges(first);
            let mut second_edges = other.ordered_edges(second);
            loop {
                budget.charge(1)?;
                match (first_edges.next(), second_edges.next()) {
                    (None, None) => break,
                    (Some(first), Some(second)) if first.weight() == second.weight() => {
                        // Charge before growing the work stack. This is a join
                        // of finite graph nodes, never an expanded type tree.
                        budget.charge(1)?;
                        pending.push((first.target(), second.target()));
                    }
                    _ => return Ok(false),
                }
            }
        }
        Ok(true)
    }
}

fn evidence_metadata_work(node: &TypeNode) -> usize {
    let identity = |name: &SymbolIdentity| name.unit.len() + name.module.len()
        + name.namespace.len() + name.occurrence.len() + name.record_parent.as_ref().map_or(0, String::len);
    match node {
        TypeNode::Root { binders, .. } => binders.len(),
        TypeNode::Declaration { identity: name, parameters, .. } => identity(name) + parameters.len(),
        TypeNode::ConstructorTemplate { identity: name, .. } => identity(name),
        TypeNode::Literal(TypeLiteral::Natural(value) | TypeLiteral::Symbol(value)) => value.len(),
        _ => 1,
    }
}

impl TypeEdge {
    fn slot(self) -> (u8, u32) {
        match self {
            Self::BinderKind(index) => (0, index), Self::Body => (1, 0),
            Self::Head => (2, 0), Self::Argument(index) => (3, index),
            Self::Function => (4, 0), Self::ApplyArgument => (5, 0),
            Self::Multiplicity => (6, 0), Self::Domain => (7, 0),
            Self::Codomain => (8, 0), Self::Kind => (9, 0),
            Self::Constructor(tag) => (10, tag),
            Self::Field { ordinal, .. } => (11, ordinal), Self::AliasRhs => (12, 0),
        }
    }
}

struct Budget { bytes: usize, work: usize, limits: GraphLimits }

impl Budget {
    fn work(&mut self, work: usize) -> Result<(), TypeGraphError> {
        self.work = self.work.checked_add(work).ok_or(TypeGraphError::Limit("work"))?;
        if self.work > self.limits.max_work { return Err(TypeGraphError::Limit("work")); }
        Ok(())
    }
    fn text(&mut self, bytes: usize) -> Result<(), TypeGraphError> {
        self.bytes = self.bytes.checked_add(bytes).ok_or(TypeGraphError::Limit("bytes"))?;
        if self.bytes > self.limits.max_bytes { return Err(TypeGraphError::Limit("bytes")); }
        self.work(bytes)
    }
    fn identity(&mut self, identity: &SymbolIdentity) -> Result<(), TypeGraphError> {
        for text in [&identity.unit, &identity.module, &identity.namespace, &identity.occurrence] {
            self.text(text.len())?;
        }
        if let Some(parent) = &identity.record_parent { self.text(parent.len())?; }
        Ok(())
    }
}

fn nominal(identity: &SymbolIdentity) -> bool {
    !identity.unit.is_empty() && !identity.module.is_empty() && !identity.occurrence.is_empty()
        && matches!(identity.namespace.as_str(), "type" | "data") && identity.record_parent.is_none()
}

fn scalar(rep: RuntimeRep) -> bool {
    matches!(rep, RuntimeRep::Int(8 | 16 | 32 | 64) | RuntimeRep::Word(8 | 16 | 32 | 64)
        | RuntimeRep::Float(32 | 64))
}

fn field_rep(rep: RuntimeRep) -> bool {
    scalar(rep) || matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address)
}

fn validate_graph(graph: &GraphStorage, constructors: &[ConstructorDecl], limits: GraphLimits)
    -> Result<usize, TypeGraphError>
{
    if graph.node_count() > limits.max_nodes { return Err(TypeGraphError::Limit("nodes")); }
    if graph.edge_count() > limits.max_edges { return Err(TypeGraphError::Limit("edges")); }
    let mut budget = Budget { bytes: 0, work: 0, limits };
    let mut declarations = BTreeSet::new();
    for node in graph.node_indices() {
        budget.work(1)?;
        let index = node.index();
        match &graph[node] {
            TypeNode::Root { domain, binders, rendered } => {
                if *domain == RootDomain::Closed && !binders.is_empty() {
                    return Err(TypeGraphError::InvalidScope(index));
                }
                budget.text(binders.len())?;
                budget.text(rendered.len())?;
            }
            TypeNode::Declaration { identity, parameters, form, .. } => {
                if !nominal(identity) { return Err(TypeGraphError::InvalidIdentity(index)); }
                if !declarations.insert(identity) { return Err(TypeGraphError::DuplicateDeclaration(index)); }
                budget.identity(identity)?;
                budget.text(parameters.len())?;
                match form {
                    DeclarationForm::Newtype { eta_arity } if *eta_arity as usize > parameters.len() =>
                        return Err(TypeGraphError::InvalidScope(index)),
                    DeclarationForm::Scalar(rep) if !scalar(*rep) =>
                        return Err(TypeGraphError::InvalidRepresentation(index)),
                    DeclarationForm::Opaque { reason, .. } => budget.text(reason.len())?,
                    _ => {}
                }
            }
            TypeNode::ConstructorTemplate { constructor, identity } => {
                let physical = constructors.get(constructor.0 as usize)
                    .ok_or(TypeGraphError::InvalidConstructor(index))?;
                if &physical.identity != identity || physical.result_rep != RuntimeRep::LiftedRef {
                    return Err(TypeGraphError::InvalidConstructor(index));
                }
                // The derived identity is not another encoded string budget.
            }
            TypeNode::Literal(TypeLiteral::Natural(value)) => {
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
                    || (value != "0" && value.starts_with('0')) {
                    return Err(TypeGraphError::InvalidLiteral(index));
                }
                budget.text(value.len())?;
            }
            TypeNode::Literal(TypeLiteral::Symbol(value)) => budget.text(value.len())?,
            _ => {}
        }
        validate_roles(graph, node, constructors, &mut budget)?;
    }
    // Only expression-child edges participate. Nominal heads and declaration
    // templates may close regular or nonregular recursion without expression
    // cycles. One DAG walk computes a free-variable requirement, not a meaning
    // assumed valid under every scope that shares the expression.
    let expressions = EdgeFiltered::from_fn(graph, |edge| {
        graph[edge.source()].is_expression() && *edge.weight() != TypeEdge::Head
    });
    budget.work(graph.node_count())?;
    budget.work(graph.edge_count())?;
    let order = petgraph::algo::toposort(&expressions, None)
        .map_err(|_| TypeGraphError::ExpressionCycle)?;
    let mut required = vec![0_usize; graph.node_count()];
    for node in order.into_iter().rev() {
        if !graph[node].is_expression() { continue; }
        budget.work(1)?;
        let need = match &graph[node] {
            TypeNode::Bound(index) => (*index as usize).checked_add(1)
                .ok_or(TypeGraphError::InvalidScope(node.index()))?,
            TypeNode::ForAll(_) => {
                let mut kind = 0; let mut body = 0;
                for edge in graph.edges(node) {
                    budget.work(1)?;
                    match edge.weight() {
                        TypeEdge::Kind => kind = required[edge.target().index()],
                        TypeEdge::Body => body = required[edge.target().index()],
                        _ => return Err(TypeGraphError::InvalidRole(node.index())),
                    }
                }
                kind.max(body.saturating_sub(1))
            }
            _ => {
                let mut maximum = 0;
                for edge in graph.edges(node).filter(|edge| *edge.weight() != TypeEdge::Head) {
                    budget.work(1)?;
                    maximum = maximum.max(required[edge.target().index()]);
                }
                maximum
            }
        };
        required[node.index()] = need;
    }
    for node in graph.node_indices() {
        match &graph[node] {
            TypeNode::Root { binders, .. } => check_scope(graph, node, binders.len(), &required, &mut budget)?,
            TypeNode::Declaration { parameters, form, .. } => {
                check_scope(graph, node, parameters.len(), &required, &mut budget)?;
                for edge in graph.edges(node) {
                    match (edge.weight(), form) {
                        (TypeEdge::AliasRhs, DeclarationForm::Newtype { eta_arity }) => {
                            if required[edge.target().index()] > *eta_arity as usize {
                                return Err(TypeGraphError::InvalidScope(node.index()));
                            }
                        }
                        (TypeEdge::Constructor(_), DeclarationForm::Data) =>
                            check_scope(graph, edge.target(), parameters.len(), &required, &mut budget)?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(budget.work)
}

fn node_evidence_equal(first: &TypeNode, second: &TypeNode, remap_constructors: bool) -> bool {
    match (first, second) {
        (TypeNode::Root { domain: first_domain, binders: first_binders, .. },
         TypeNode::Root { domain: second_domain, binders: second_binders, .. }) =>
            first_domain == second_domain && first_binders == second_binders,
        (TypeNode::Declaration { identity: first_identity, parameters: first_parameters,
            form: first_form, restriction: first_restriction },
         TypeNode::Declaration { identity: second_identity, parameters: second_parameters,
            form: second_form, restriction: second_restriction }) => {
            let forms = match (first_form, second_form) {
                (DeclarationForm::Opaque { head_kind: first, .. }, DeclarationForm::Opaque { head_kind: second, .. }) =>
                    first == second,
                _ => first_form == second_form,
            };
            first_identity == second_identity && first_parameters == second_parameters
                && first_restriction == second_restriction && forms
        }
        (TypeNode::ConstructorTemplate { constructor: first_constructor, identity: first_identity },
         TypeNode::ConstructorTemplate { constructor: second_constructor, identity: second_identity }) =>
            first_identity == second_identity && (remap_constructors || first_constructor == second_constructor),
        _ => first == second,
    }
}

fn write_rep(rep: RuntimeRep, frame: &mut impl FnMut(&[u8])) {
    match rep {
        RuntimeRep::Void => frame(&[0]), RuntimeRep::LiftedRef => frame(&[1]),
        RuntimeRep::UnliftedRef => frame(&[2]), RuntimeRep::Address => frame(&[3]),
        RuntimeRep::Int(width) => frame(&[4, width]), RuntimeRep::Word(width) => frame(&[5, width]),
        RuntimeRep::Float(width) => frame(&[6, width]),
    }
}

fn write_symbol(identity: &SymbolIdentity, frame: &mut impl FnMut(&[u8])) {
    for text in [&identity.unit, &identity.module, &identity.namespace, &identity.occurrence] {
        frame(text.as_bytes());
    }
    match &identity.record_parent { None => frame(&[0]), Some(parent) => { frame(&[1]); frame(parent.as_bytes()); } }
}

fn write_node(node: &TypeNode, evidence: bool, frame: &mut impl FnMut(&[u8])) {
    match node {
        TypeNode::Root { domain, binders, rendered } => {
            frame(&[0]); frame(&[match domain { RootDomain::Closed => 0, RootDomain::ConstructorScheme => 1 }]);
            frame(&(binders.len() as u64).to_le_bytes());
            for flag in binders { frame(&[match flag { SourceBinderFlag::Specified => 1, SourceBinderFlag::Inferred => 2 }]); }
            if !evidence { frame(rendered.as_bytes()); }
        }
        TypeNode::Declaration { identity, parameters, form, restriction } => {
            frame(&[1]); write_symbol(identity, frame);
            frame(&(parameters.len() as u64).to_le_bytes());
            for flag in parameters { frame(&[match flag { ParameterFlag::NamedRequired => 0,
                ParameterFlag::NamedSpecified => 1, ParameterFlag::NamedInferred => 2,
                ParameterFlag::AnonymousVisible => 3 }]); }
            match form {
                DeclarationForm::Data => frame(&[0]),
                DeclarationForm::Newtype { eta_arity } => { frame(&[1]); frame(&eta_arity.to_le_bytes()); }
                DeclarationForm::Text => frame(&[2]), DeclarationForm::Integer => frame(&[3]),
                DeclarationForm::Natural => frame(&[4]), DeclarationForm::Scalar(rep) => { frame(&[5]); write_rep(*rep, frame); }
                DeclarationForm::Opaque { head_kind, reason } => {
                    frame(&[6]); frame(&[match head_kind { NominalHeadKind::Constructor => 0, NominalHeadKind::Family => 1 }]);
                    if !evidence { frame(reason.as_bytes()); }
                }
            }
            frame(&[match restriction { SyntaxRestriction::None => 0, SyntaxRestriction::EffectHead => 1 }]);
        }
        TypeNode::ConstructorTemplate { constructor, identity } => {
            frame(&[2]); frame(&constructor.0.to_le_bytes()); write_symbol(identity, frame);
        }
        TypeNode::Bound(index) => { frame(&[3]); frame(&index.to_le_bytes()); }
        TypeNode::NominalApplication => frame(&[4]), TypeNode::Application => frame(&[5]),
        TypeNode::Function(flag) => { frame(&[6]); frame(&[match flag { FunctionFlag::TypeToType => 0,
            FunctionFlag::TypeToConstraint => 1, FunctionFlag::ConstraintToType => 2, FunctionFlag::ConstraintToConstraint => 3 }]); }
        TypeNode::ForAll(flag) => { frame(&[7]); frame(&[match flag { ForAllFlag::Required => 0,
            ForAllFlag::Specified => 1, ForAllFlag::Inferred => 2 }]); }
        TypeNode::Literal(literal) => {
            frame(&[8]); match literal {
                TypeLiteral::Natural(value) => { frame(&[0]); frame(value.as_bytes()); }
                TypeLiteral::Symbol(value) => { frame(&[1]); frame(value.as_bytes()); }
                TypeLiteral::Character(value) => { frame(&[2]); frame(&(*value as u32).to_le_bytes()); }
            }
        }
    }
}

fn validate_roles(graph: &GraphStorage, node: TypeNodeId, constructors: &[ConstructorDecl], budget: &mut Budget)
    -> Result<(), TypeGraphError>
{
    let index = node.index();
    let mut slots = BTreeSet::new();
    for edge in graph.edges(node) {
        budget.work(1)?;
        if !slots.insert(edge.weight().slot()) { return Err(TypeGraphError::InvalidCardinality(index)); }
        let target = graph.node_weight(edge.target()).ok_or(TypeGraphError::InvalidReference(index))?;
        let expression = target.is_expression();
        let allowed = match (&graph[node], edge.weight()) {
            (TypeNode::Root { .. }, TypeEdge::Body | TypeEdge::BinderKind(_)) => expression,
            (TypeNode::Declaration { .. }, TypeEdge::BinderKind(_)) => expression,
            (TypeNode::Declaration { form: DeclarationForm::Newtype { .. }, .. }, TypeEdge::AliasRhs) => expression,
            (TypeNode::Declaration { form: DeclarationForm::Data, .. }, TypeEdge::Constructor(_)) =>
                matches!(target, TypeNode::ConstructorTemplate { .. }),
            (TypeNode::ConstructorTemplate { .. }, TypeEdge::Field { source_rep, .. }) => {
                if !field_rep(*source_rep) { return Err(TypeGraphError::InvalidRepresentation(index)); }
                expression
            }
            (TypeNode::NominalApplication, TypeEdge::Head) => matches!(target, TypeNode::Declaration { .. }),
            (TypeNode::NominalApplication, TypeEdge::Argument(_)) => expression,
            (TypeNode::Application, TypeEdge::Function | TypeEdge::ApplyArgument) => expression,
            (TypeNode::Function(_), TypeEdge::Multiplicity | TypeEdge::Domain | TypeEdge::Codomain) => expression,
            (TypeNode::ForAll(_), TypeEdge::Kind | TypeEdge::Body) => expression,
            _ => false,
        };
        if !allowed { return Err(TypeGraphError::InvalidRole(index)); }
    }
    let contiguous = |tag: u8, count: usize| slots.iter().filter(|slot| slot.0 == tag)
        .map(|slot| slot.1 as usize).eq(0..count);
    let exact = |expected: &[(u8, u32)]| slots.iter().copied().eq(expected.iter().copied());
    let valid = match &graph[node] {
        TypeNode::Root { binders, .. } => slots.len() == binders.len() + 1
            && slots.contains(&(1, 0)) && contiguous(0, binders.len()),
        TypeNode::Declaration { parameters, form, .. } => {
            let kinds = contiguous(0, parameters.len());
            match form {
                DeclarationForm::Data => kinds,
                DeclarationForm::Newtype { .. } => kinds && slots.len() == parameters.len() + 1
                    && slots.contains(&(12, 0)),
                _ => kinds && slots.len() == parameters.len(),
            }
        }
        TypeNode::ConstructorTemplate { constructor, .. } => {
            let physical = constructors.get(constructor.0 as usize)
                .ok_or(TypeGraphError::InvalidConstructor(index))?;
            if !contiguous(11, physical.field_reps.len()) || slots.len() != physical.field_reps.len() {
                return Err(TypeGraphError::InvalidConstructor(index));
            }
            for edge in graph.edges(node) {
                if let TypeEdge::Field { ordinal, source_rep } = edge.weight() {
                    if physical.field_reps.get(*ordinal as usize) != Some(source_rep) {
                        return Err(TypeGraphError::InvalidConstructor(index));
                    }
                }
            }
            let mut incoming = graph.edges_directed(node, petgraph::Incoming);
            let parent = incoming.next().ok_or(TypeGraphError::InvalidConstructor(index))?;
            if incoming.next().is_some() { return Err(TypeGraphError::InvalidConstructor(index)); }
            match (&graph[parent.source()], parent.weight()) {
                (TypeNode::Declaration { identity, form: DeclarationForm::Data, .. }, TypeEdge::Constructor(tag))
                    if identity == &physical.family && *tag == physical.tag && *tag > 0 => {}
                _ => return Err(TypeGraphError::InvalidConstructor(index)),
            }
            true
        }
        TypeNode::NominalApplication => slots.contains(&(2, 0))
            && contiguous(3, slots.len().saturating_sub(1)),
        TypeNode::Application => exact(&[(4, 0), (5, 0)]),
        TypeNode::Function(_) => exact(&[(6, 0), (7, 0), (8, 0)]),
        TypeNode::ForAll(_) => exact(&[(1, 0), (9, 0)]),
        TypeNode::Bound(_) | TypeNode::Literal(_) => slots.is_empty(),
    };
    if !valid { return Err(TypeGraphError::InvalidCardinality(index)); }
    if let TypeNode::Declaration { form: DeclarationForm::Data, .. } = &graph[node] {
        let mut count = 0_usize;
        let expected = slots.iter().filter(|slot| slot.0 == 10).count();
        for edge in graph.edges(node).filter(|edge| matches!(edge.weight(), TypeEdge::Constructor(_))) {
            let TypeNode::ConstructorTemplate { constructor, .. } = &graph[edge.target()] else {
                return Err(TypeGraphError::InvalidConstructor(index));
            };
            let physical = constructors.get(constructor.0 as usize)
                .ok_or(TypeGraphError::InvalidConstructor(index))?;
            if physical.family_size as usize != expected || physical.tag as usize > expected {
                return Err(TypeGraphError::InvalidConstructor(index));
            }
            count += 1;
        }
        if !slots.iter().filter(|slot| slot.0 == 10).map(|slot| slot.1 as usize).eq(1..=count) {
            return Err(TypeGraphError::InvalidConstructor(index));
        }
    }
    Ok(())
}

fn check_scope(graph: &GraphStorage, node: TypeNodeId, binders: usize, required: &[usize], budget: &mut Budget)
    -> Result<(), TypeGraphError>
{
    for edge in graph.edges(node) {
        budget.work(1)?;
        let available = match edge.weight() {
            TypeEdge::BinderKind(position) => *position as usize,
            TypeEdge::Body | TypeEdge::Field { .. } => binders,
            _ => continue,
        };
        if required[edge.target().index()] > available { return Err(TypeGraphError::InvalidScope(node.index())); }
    }
    Ok(())
}
