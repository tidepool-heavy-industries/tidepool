//! Finite compiler type declarations and scoped expressions.
//!
//! Node weights retain metadata; all child relationships live in the graph.
//! Original compiler admission and physical constructor authority remain with
//! their existing owners. A graph describes type evidence, not authority.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

use petgraph::graph::{Edges, Graph, NodeIndex};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::Directed;

use crate::execution_schema::{
    ConstructorDecl, ConstructorId, DecodeLimits, RuntimeRep, SymbolIdentity,
};

mod cursor;
pub use cursor::{ConstructionRefusal, DataView, TypeCursor, TypeView};

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
    Newtype {
        eta_arity: u32,
    },
    Text,
    Integer,
    Natural,
    Scalar(RuntimeRep),
    Opaque {
        head_kind: NominalHeadKind,
        reason: String,
    },
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
        !matches!(
            self,
            Self::Root { .. } | Self::Declaration { .. } | Self::ConstructorTemplate { .. }
        )
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
    Field {
        ordinal: u32,
        source_rep: RuntimeRep,
    },
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
        let mut budget = Budget {
            bytes: 0,
            work,
            limits,
        };
        // petgraph's adjacency lists reverse insertion order. Canonicalize
        // once, preserving node indices and repeated targets at distinct slots.
        let count = graph.edge_count();
        // Reserve a conservative comparison bound before entering standard
        // sort, which cannot return a budget error from its comparator. Keep
        // this reservation in the reported work charged by the decoder.
        let levels = if count < 2 {
            0
        } else {
            usize::BITS as usize - (count - 1).leading_zeros() as usize
        };
        let sort_work = count
            .checked_mul(levels)
            .and_then(|work| work.checked_mul(8))
            .ok_or(TypeGraphError::Limit("work"))?;
        budget.work(count)?;
        budget.work(sort_work)?;
        let mut edges = Vec::with_capacity(count);
        for edge in graph.edge_references() {
            edges.push((edge.source(), edge.target(), *edge.weight()));
        }
        edges.sort_unstable_by(|first, second| {
            first
                .0
                .index()
                .cmp(&second.0.index())
                .then_with(|| second.2.cmp(&first.2))
        });
        budget.work(count)?;
        graph.clear_edges();
        for (source, target, role) in edges {
            graph.add_edge(source, target, role);
        }
        Ok((Self { graph }, budget.work))
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
    pub fn check_constructor_pairing(
        &self,
        constructors: &[ConstructorDecl],
    ) -> Result<(), TypeGraphError> {
        self.check_constructor_pairing_with_work(constructors, GraphLimits::default())
            .map(|_| ())
    }

    pub fn check_constructor_pairing_with_work(
        &self,
        constructors: &[ConstructorDecl],
        limits: GraphLimits,
    ) -> Result<usize, TypeGraphError> {
        let mut budget = Budget {
            bytes: 0,
            work: 0,
            limits,
        };
        let families = family_counts(constructors, &mut budget)?;
        for node in self.graph.node_indices() {
            budget.work(1)?;
            match &self.graph[node] {
                TypeNode::ConstructorTemplate {
                    constructor,
                    identity,
                } => {
                    let physical = constructors
                        .get(constructor.0 as usize)
                        .ok_or(TypeGraphError::InvalidConstructor(node.index()))?;
                    let mut field_count = 0;
                    for _ in self.graph.edges(node) {
                        budget.work(1)?;
                        field_count += 1;
                    }
                    if !budget.identity_eq(identity, &physical.identity)?
                        || physical.result_rep != RuntimeRep::LiftedRef
                        || physical.field_reps.len() != field_count
                    {
                        return Err(TypeGraphError::InvalidConstructor(node.index()));
                    }
                    let parent = self
                        .graph
                        .edges_directed(node, petgraph::Incoming)
                        .next()
                        .ok_or(TypeGraphError::InvalidConstructor(node.index()))?;
                    match (&self.graph[parent.source()], parent.weight()) {
                        (TypeNode::Declaration { identity, .. }, TypeEdge::Constructor(tag))
                            if budget.identity_eq(identity, &physical.family)?
                                && *tag == physical.tag => {}
                        _ => return Err(TypeGraphError::InvalidConstructor(node.index())),
                    }
                    for edge in self.ordered_edges(node) {
                        budget.work(1)?;
                        match edge.weight() {
                            TypeEdge::Field {
                                ordinal,
                                source_rep,
                            } if physical.field_reps.get(*ordinal as usize) == Some(source_rep) => {
                            }
                            _ => return Err(TypeGraphError::InvalidConstructor(node.index())),
                        }
                    }
                }
                TypeNode::Declaration {
                    identity,
                    form: DeclarationForm::Data,
                    ..
                } => {
                    let mut count = 0;
                    for edge in self.graph.edges(node) {
                        budget.work(1)?;
                        if matches!(edge.weight(), TypeEdge::Constructor(_)) {
                            count += 1;
                        }
                    }
                    budget.work(symbol_work(identity))?;
                    if families.get(identity).copied().unwrap_or(0) != count {
                        return Err(TypeGraphError::InvalidConstructor(node.index()));
                    }
                    for edge in self.ordered_edges(node) {
                        budget.work(1)?;
                        if matches!(edge.weight(), TypeEdge::Constructor(_)) {
                            let TypeNode::ConstructorTemplate { constructor, .. } =
                                &self.graph[edge.target()]
                            else {
                                return Err(TypeGraphError::InvalidConstructor(node.index()));
                            };
                            if constructors
                                .get(constructor.0 as usize)
                                .is_none_or(|physical| physical.family_size as usize != count)
                            {
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

    pub fn content_eq(&self, other: &Self) -> bool {
        self.ordered_eq(other, false)
    }

    /// Whole ordered graph identity, excluding only diagnostic renderings and
    /// opaque reasons. Root, node and edge positions remain part of identity.
    pub fn evidence_eq(&self, other: &Self) -> bool {
        self.ordered_eq(other, true)
    }

    fn ordered_eq(&self, other: &Self, evidence: bool) -> bool {
        self.graph.node_count() == other.graph.node_count()
            && self.graph.edge_count() == other.graph.edge_count()
            && self.graph.node_indices().all(|node| {
                let metadata = if evidence {
                    node_evidence_equal(&self.graph[node], &other.graph[node], false)
                } else {
                    self.graph[node] == other.graph[node]
                };
                metadata
                    && self
                        .ordered_edges(node)
                        .map(|edge| (edge.target(), *edge.weight()))
                        .eq(other
                            .ordered_edges(node)
                            .map(|edge| (edge.target(), *edge.weight())))
            })
    }

    /// Stable explicit framing for the ordered graph's evidence commitment.
    /// The enclosing request owner adds its constructor inventory, endpoints
    /// and authenticated context to its existing commitment domain.
    pub fn write_evidence(&self, sink: impl FnMut(&[u8])) {
        self.write_identity(sink, true);
    }

    pub fn write_content(&self, sink: impl FnMut(&[u8])) {
        self.write_identity(sink, false);
    }

    fn write_identity(&self, mut sink: impl FnMut(&[u8]), evidence: bool) {
        let mut frame = |bytes: &[u8]| {
            sink(&(bytes.len() as u64).to_le_bytes());
            sink(bytes);
        };
        frame(if evidence {
            b"Tidepool.TypeGraph.evidence.v1"
        } else {
            b"Tidepool.TypeGraph.content.v1"
        });
        frame(&(self.graph.node_count() as u64).to_le_bytes());
        frame(&(self.graph.edge_count() as u64).to_le_bytes());
        for node in self.graph.node_indices() {
            write_node(&self.graph[node], evidence, &mut frame);
            frame(&(self.graph.edges(node).count() as u64).to_le_bytes());
            for edge in self.ordered_edges(node) {
                frame(&(edge.target().index() as u32).to_le_bytes());
                let (tag, index) = edge.weight().slot();
                frame(&[tag]);
                frame(&index.to_le_bytes());
                if let TypeEdge::Field { source_rep, .. } = edge.weight() {
                    write_rep(*source_rep, &mut frame);
                }
            }
        }
    }
}

impl Default for TypeGraph {
    fn default() -> Self {
        Self {
            graph: GraphStorage::new(),
        }
    }
}

impl PartialEq for TypeGraph {
    fn eq(&self, other: &Self) -> bool {
        self.content_eq(other)
    }
}
impl Eq for TypeGraph {}
impl Hash for TypeGraph {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.write_content(|bytes| state.write(bytes));
    }
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
        Self {
            max_nodes: limits.max_type_nodes,
            max_edges: limits.max_work,
            max_bytes: limits.max_bytes,
            max_work: limits.max_work,
        }
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
pub struct TypeWorkBudget {
    limit: usize,
    spent: usize,
}

impl TypeWorkBudget {
    pub fn new(limit: usize) -> Self {
        Self { limit, spent: 0 }
    }
    pub fn charge(&mut self, work: usize) -> Result<(), TypeGraphError> {
        let spent = self
            .spent
            .checked_add(work)
            .ok_or(TypeGraphError::TraversalWork)?;
        if spent > self.limit {
            return Err(TypeGraphError::TraversalWork);
        }
        self.spent = spent;
        Ok(())
    }
    pub fn spent(&self) -> usize {
        self.spent
    }
    pub fn remaining(&self) -> usize {
        self.limit - self.spent
    }
}

impl TypeGraph {
    /// Finite exact scoped type identity, independent of storage indices and
    /// diagnostics. Declaration templates compare under their formal telescope;
    /// this walk never instantiates recursively growing constructor fields.
    pub fn rooted_identity_eq(
        &self,
        root: TypeNodeId,
        other: &Self,
        other_root: TypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
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
    pub fn declaration_identity_eq(
        &self,
        declaration: TypeNodeId,
        other: &Self,
        other_declaration: TypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        for (graph, node) in [(self, declaration), (other, other_declaration)] {
            if !matches!(
                graph.graph.node_weight(node),
                Some(TypeNode::Declaration { .. })
            ) {
                return Err(TypeGraphError::InvalidRole(node.index()));
            }
        }
        self.node_identity_eq(declaration, other, other_declaration, budget)
    }

    fn node_identity_eq(
        &self,
        first: TypeNodeId,
        other: &Self,
        second: TypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        let mut pending = vec![(first, second)];
        let mut visited = BTreeSet::new();
        while let Some((first, second)) = pending.pop() {
            budget.charge(1)?;
            if !visited.insert((first.index(), second.index())) {
                continue;
            }
            let first_node = self
                .graph
                .node_weight(first)
                .ok_or(TypeGraphError::InvalidReference(first.index()))?;
            let second_node = other
                .graph
                .node_weight(second)
                .ok_or(TypeGraphError::InvalidReference(second.index()))?;
            budget.charge(
                evidence_metadata_work(first_node).max(evidence_metadata_work(second_node)),
            )?;
            if !node_evidence_equal(first_node, second_node, true) {
                return Ok(false);
            }
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
    match node {
        TypeNode::Root { binders, .. } => binders.len(),
        TypeNode::Declaration {
            identity: name,
            parameters,
            ..
        } => symbol_work(name).saturating_add(parameters.len()),
        TypeNode::ConstructorTemplate { identity: name, .. } => symbol_work(name),
        TypeNode::Literal(TypeLiteral::Natural(value) | TypeLiteral::Symbol(value)) => value.len(),
        _ => 1,
    }
}

fn symbol_work(name: &SymbolIdentity) -> usize {
    [&name.unit, &name.module, &name.namespace, &name.occurrence]
        .into_iter()
        .chain(name.record_parent.iter())
        .fold(0_usize, |work, text| work.saturating_add(text.len()))
}

impl TypeEdge {
    fn slot(self) -> (u8, u32) {
        match self {
            Self::BinderKind(index) => (0, index),
            Self::Body => (1, 0),
            Self::Head => (2, 0),
            Self::Argument(index) => (3, index),
            Self::Function => (4, 0),
            Self::ApplyArgument => (5, 0),
            Self::Multiplicity => (6, 0),
            Self::Domain => (7, 0),
            Self::Codomain => (8, 0),
            Self::Kind => (9, 0),
            Self::Constructor(tag) => (10, tag),
            Self::Field { ordinal, .. } => (11, ordinal),
            Self::AliasRhs => (12, 0),
        }
    }
}

struct Budget {
    bytes: usize,
    work: usize,
    limits: GraphLimits,
}

impl Budget {
    fn work(&mut self, work: usize) -> Result<(), TypeGraphError> {
        self.work = self
            .work
            .checked_add(work)
            .ok_or(TypeGraphError::Limit("work"))?;
        if self.work > self.limits.max_work {
            return Err(TypeGraphError::Limit("work"));
        }
        Ok(())
    }
    fn text(&mut self, bytes: usize) -> Result<(), TypeGraphError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(TypeGraphError::Limit("bytes"))?;
        if self.bytes > self.limits.max_bytes {
            return Err(TypeGraphError::Limit("bytes"));
        }
        self.work(bytes)
    }
    fn identity(&mut self, identity: &SymbolIdentity) -> Result<(), TypeGraphError> {
        for text in [
            &identity.unit,
            &identity.module,
            &identity.namespace,
            &identity.occurrence,
        ] {
            self.text(text.len())?;
        }
        if let Some(parent) = &identity.record_parent {
            self.text(parent.len())?;
        }
        Ok(())
    }
    fn identity_eq(
        &mut self,
        first: &SymbolIdentity,
        second: &SymbolIdentity,
    ) -> Result<bool, TypeGraphError> {
        self.work(symbol_work(first).max(symbol_work(second)))?;
        Ok(first == second)
    }
}

fn nominal(identity: &SymbolIdentity) -> bool {
    !identity.unit.is_empty()
        && !identity.module.is_empty()
        && !identity.occurrence.is_empty()
        && matches!(identity.namespace.as_str(), "type" | "data")
        && identity.record_parent.is_none()
}

fn scalar(rep: RuntimeRep) -> bool {
    matches!(
        rep,
        RuntimeRep::Int(8 | 16 | 32 | 64)
            | RuntimeRep::Word(8 | 16 | 32 | 64)
            | RuntimeRep::Float(32 | 64)
    )
}

fn field_rep(rep: RuntimeRep) -> bool {
    scalar(rep)
        || matches!(
            rep,
            RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address
        )
}

fn family_counts<'a>(
    constructors: &'a [ConstructorDecl],
    budget: &mut Budget,
) -> Result<HashMap<&'a SymbolIdentity, usize>, TypeGraphError> {
    let mut families = HashMap::new();
    for constructor in constructors {
        budget.work(1)?;
        budget.work(symbol_work(&constructor.family))?;
        let count = families.entry(&constructor.family).or_insert(0_usize);
        *count = count.checked_add(1).ok_or(TypeGraphError::Limit("work"))?;
    }
    Ok(families)
}

fn validate_graph(
    graph: &GraphStorage,
    constructors: &[ConstructorDecl],
    limits: GraphLimits,
) -> Result<usize, TypeGraphError> {
    if graph.node_count() > limits.max_nodes {
        return Err(TypeGraphError::Limit("nodes"));
    }
    if graph.edge_count() > limits.max_edges {
        return Err(TypeGraphError::Limit("edges"));
    }
    let mut budget = Budget {
        bytes: 0,
        work: 0,
        limits,
    };
    let families = family_counts(constructors, &mut budget)?;
    let mut declarations = BTreeSet::new();
    let mut templates = BTreeSet::new();
    for node in graph.node_indices() {
        budget.work(1)?;
        let index = node.index();
        match &graph[node] {
            TypeNode::Root {
                domain,
                binders,
                rendered,
            } => {
                if *domain == RootDomain::Closed && !binders.is_empty() {
                    return Err(TypeGraphError::InvalidScope(index));
                }
                budget.text(binders.len())?;
                budget.text(rendered.len())?;
            }
            TypeNode::Declaration {
                identity,
                parameters,
                form,
                ..
            } => {
                if !nominal(identity) {
                    return Err(TypeGraphError::InvalidIdentity(index));
                }
                budget.identity(identity)?;
                if !declarations.insert(identity) {
                    return Err(TypeGraphError::DuplicateDeclaration(index));
                }
                budget.text(parameters.len())?;
                match form {
                    DeclarationForm::Newtype { eta_arity }
                        if *eta_arity as usize > parameters.len() =>
                    {
                        return Err(TypeGraphError::InvalidScope(index))
                    }
                    DeclarationForm::Scalar(rep) if !scalar(*rep) => {
                        return Err(TypeGraphError::InvalidRepresentation(index))
                    }
                    DeclarationForm::Opaque { reason, .. } => budget.text(reason.len())?,
                    _ => {}
                }
            }
            TypeNode::ConstructorTemplate {
                constructor,
                identity,
            } => {
                if !templates.insert(constructor.0) {
                    return Err(TypeGraphError::InvalidConstructor(index));
                }
                let physical = constructors
                    .get(constructor.0 as usize)
                    .ok_or(TypeGraphError::InvalidConstructor(index))?;
                if !budget.identity_eq(identity, &physical.identity)?
                    || physical.result_rep != RuntimeRep::LiftedRef
                {
                    return Err(TypeGraphError::InvalidConstructor(index));
                }
                // The derived identity is not another encoded string budget.
            }
            TypeNode::Literal(TypeLiteral::Natural(value)) => {
                budget.text(value.len())?;
                if value.is_empty()
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                    || (value != "0" && value.starts_with('0'))
                {
                    return Err(TypeGraphError::InvalidLiteral(index));
                }
            }
            TypeNode::Literal(TypeLiteral::Symbol(value)) => budget.text(value.len())?,
            _ => {}
        }
        validate_roles(graph, node, constructors, &families, &mut budget)?;
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
        if !graph[node].is_expression() {
            continue;
        }
        budget.work(1)?;
        let need = match &graph[node] {
            TypeNode::Bound(index) => (*index as usize)
                .checked_add(1)
                .ok_or(TypeGraphError::InvalidScope(node.index()))?,
            TypeNode::ForAll(_) => {
                let mut kind = 0;
                let mut body = 0;
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
                for edge in graph
                    .edges(node)
                    .filter(|edge| *edge.weight() != TypeEdge::Head)
                {
                    budget.work(1)?;
                    maximum = maximum.max(required[edge.target().index()]);
                }
                maximum
            }
        };
        required[node.index()] = need;
    }
    for node in graph.node_indices() {
        budget.work(1)?;
        match &graph[node] {
            TypeNode::Root { binders, .. } => {
                check_scope(graph, node, binders.len(), &required, &mut budget)?
            }
            TypeNode::Declaration {
                parameters, form, ..
            } => {
                check_scope(graph, node, parameters.len(), &required, &mut budget)?;
                for edge in graph.edges(node) {
                    budget.work(1)?;
                    match (edge.weight(), form) {
                        (TypeEdge::AliasRhs, DeclarationForm::Newtype { eta_arity }) => {
                            if required[edge.target().index()] > *eta_arity as usize {
                                return Err(TypeGraphError::InvalidScope(node.index()));
                            }
                        }
                        (TypeEdge::Constructor(_), DeclarationForm::Data) => check_scope(
                            graph,
                            edge.target(),
                            parameters.len(),
                            &required,
                            &mut budget,
                        )?,
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
        (
            TypeNode::Root {
                domain: first_domain,
                binders: first_binders,
                ..
            },
            TypeNode::Root {
                domain: second_domain,
                binders: second_binders,
                ..
            },
        ) => first_domain == second_domain && first_binders == second_binders,
        (
            TypeNode::Declaration {
                identity: first_identity,
                parameters: first_parameters,
                form: first_form,
                restriction: first_restriction,
            },
            TypeNode::Declaration {
                identity: second_identity,
                parameters: second_parameters,
                form: second_form,
                restriction: second_restriction,
            },
        ) => {
            let forms = match (first_form, second_form) {
                (
                    DeclarationForm::Opaque {
                        head_kind: first, ..
                    },
                    DeclarationForm::Opaque {
                        head_kind: second, ..
                    },
                ) => first == second,
                _ => first_form == second_form,
            };
            first_identity == second_identity
                && first_parameters == second_parameters
                && first_restriction == second_restriction
                && forms
        }
        (
            TypeNode::ConstructorTemplate {
                constructor: first_constructor,
                identity: first_identity,
            },
            TypeNode::ConstructorTemplate {
                constructor: second_constructor,
                identity: second_identity,
            },
        ) => {
            first_identity == second_identity
                && (remap_constructors || first_constructor == second_constructor)
        }
        _ => first == second,
    }
}

fn write_rep(rep: RuntimeRep, frame: &mut impl FnMut(&[u8])) {
    match rep {
        RuntimeRep::Void => frame(&[0]),
        RuntimeRep::LiftedRef => frame(&[1]),
        RuntimeRep::UnliftedRef => frame(&[2]),
        RuntimeRep::Address => frame(&[3]),
        RuntimeRep::Int(width) => frame(&[4, width]),
        RuntimeRep::Word(width) => frame(&[5, width]),
        RuntimeRep::Float(width) => frame(&[6, width]),
    }
}

fn write_symbol(identity: &SymbolIdentity, frame: &mut impl FnMut(&[u8])) {
    for text in [
        &identity.unit,
        &identity.module,
        &identity.namespace,
        &identity.occurrence,
    ] {
        frame(text.as_bytes());
    }
    match &identity.record_parent {
        None => frame(&[0]),
        Some(parent) => {
            frame(&[1]);
            frame(parent.as_bytes());
        }
    }
}

fn write_node(node: &TypeNode, evidence: bool, frame: &mut impl FnMut(&[u8])) {
    match node {
        TypeNode::Root {
            domain,
            binders,
            rendered,
        } => {
            frame(&[0]);
            frame(&[match domain {
                RootDomain::Closed => 0,
                RootDomain::ConstructorScheme => 1,
            }]);
            frame(&(binders.len() as u64).to_le_bytes());
            for flag in binders {
                frame(&[match flag {
                    SourceBinderFlag::Specified => 1,
                    SourceBinderFlag::Inferred => 2,
                }]);
            }
            if !evidence {
                frame(rendered.as_bytes());
            }
        }
        TypeNode::Declaration {
            identity,
            parameters,
            form,
            restriction,
        } => {
            frame(&[1]);
            write_symbol(identity, frame);
            frame(&(parameters.len() as u64).to_le_bytes());
            for flag in parameters {
                frame(&[match flag {
                    ParameterFlag::NamedRequired => 0,
                    ParameterFlag::NamedSpecified => 1,
                    ParameterFlag::NamedInferred => 2,
                    ParameterFlag::AnonymousVisible => 3,
                }]);
            }
            match form {
                DeclarationForm::Data => frame(&[0]),
                DeclarationForm::Newtype { eta_arity } => {
                    frame(&[1]);
                    frame(&eta_arity.to_le_bytes());
                }
                DeclarationForm::Text => frame(&[2]),
                DeclarationForm::Integer => frame(&[3]),
                DeclarationForm::Natural => frame(&[4]),
                DeclarationForm::Scalar(rep) => {
                    frame(&[5]);
                    write_rep(*rep, frame);
                }
                DeclarationForm::Opaque { head_kind, reason } => {
                    frame(&[6]);
                    frame(&[match head_kind {
                        NominalHeadKind::Constructor => 0,
                        NominalHeadKind::Family => 1,
                    }]);
                    if !evidence {
                        frame(reason.as_bytes());
                    }
                }
            }
            frame(&[match restriction {
                SyntaxRestriction::None => 0,
                SyntaxRestriction::EffectHead => 1,
            }]);
        }
        TypeNode::ConstructorTemplate {
            constructor,
            identity,
        } => {
            frame(&[2]);
            frame(&constructor.0.to_le_bytes());
            write_symbol(identity, frame);
        }
        TypeNode::Bound(index) => {
            frame(&[3]);
            frame(&index.to_le_bytes());
        }
        TypeNode::NominalApplication => frame(&[4]),
        TypeNode::Application => frame(&[5]),
        TypeNode::Function(flag) => {
            frame(&[6]);
            frame(&[match flag {
                FunctionFlag::TypeToType => 0,
                FunctionFlag::TypeToConstraint => 1,
                FunctionFlag::ConstraintToType => 2,
                FunctionFlag::ConstraintToConstraint => 3,
            }]);
        }
        TypeNode::ForAll(flag) => {
            frame(&[7]);
            frame(&[match flag {
                ForAllFlag::Required => 0,
                ForAllFlag::Specified => 1,
                ForAllFlag::Inferred => 2,
            }]);
        }
        TypeNode::Literal(literal) => {
            frame(&[8]);
            match literal {
                TypeLiteral::Natural(value) => {
                    frame(&[0]);
                    frame(value.as_bytes());
                }
                TypeLiteral::Symbol(value) => {
                    frame(&[1]);
                    frame(value.as_bytes());
                }
                TypeLiteral::Character(value) => {
                    frame(&[2]);
                    frame(&(*value as u32).to_le_bytes());
                }
            }
        }
    }
}

fn validate_roles(
    graph: &GraphStorage,
    node: TypeNodeId,
    constructors: &[ConstructorDecl],
    families: &HashMap<&SymbolIdentity, usize>,
    budget: &mut Budget,
) -> Result<(), TypeGraphError> {
    let index = node.index();
    let mut slots = BTreeSet::new();
    for edge in graph.edges(node) {
        budget.work(1)?;
        if !slots.insert(edge.weight().slot()) {
            return Err(TypeGraphError::InvalidCardinality(index));
        }
        let target = graph
            .node_weight(edge.target())
            .ok_or(TypeGraphError::InvalidReference(index))?;
        let expression = target.is_expression();
        let allowed = match (&graph[node], edge.weight()) {
            (TypeNode::Root { .. }, TypeEdge::Body | TypeEdge::BinderKind(_)) => expression,
            (TypeNode::Declaration { .. }, TypeEdge::BinderKind(_)) => expression,
            (
                TypeNode::Declaration {
                    form: DeclarationForm::Newtype { .. },
                    ..
                },
                TypeEdge::AliasRhs,
            ) => expression,
            (
                TypeNode::Declaration {
                    form: DeclarationForm::Data,
                    ..
                },
                TypeEdge::Constructor(_),
            ) => matches!(target, TypeNode::ConstructorTemplate { .. }),
            (TypeNode::ConstructorTemplate { .. }, TypeEdge::Field { source_rep, .. }) => {
                if !field_rep(*source_rep) {
                    return Err(TypeGraphError::InvalidRepresentation(index));
                }
                expression
            }
            (TypeNode::NominalApplication, TypeEdge::Head) => {
                matches!(target, TypeNode::Declaration { .. })
            }
            (TypeNode::NominalApplication, TypeEdge::Argument(_)) => expression,
            (TypeNode::Application, TypeEdge::Function | TypeEdge::ApplyArgument) => expression,
            (
                TypeNode::Function(_),
                TypeEdge::Multiplicity | TypeEdge::Domain | TypeEdge::Codomain,
            ) => expression,
            (TypeNode::ForAll(_), TypeEdge::Kind | TypeEdge::Body) => expression,
            _ => false,
        };
        if !allowed {
            return Err(TypeGraphError::InvalidRole(index));
        }
    }
    let contiguous = |tag: u8, count: usize| {
        slots
            .iter()
            .filter(|slot| slot.0 == tag)
            .map(|slot| slot.1 as usize)
            .eq(0..count)
    };
    let exact = |expected: &[(u8, u32)]| slots.iter().copied().eq(expected.iter().copied());
    let valid = match &graph[node] {
        TypeNode::Root { binders, .. } => {
            slots.len() == binders.len() + 1
                && slots.contains(&(1, 0))
                && contiguous(0, binders.len())
        }
        TypeNode::Declaration {
            parameters, form, ..
        } => {
            let kinds = contiguous(0, parameters.len());
            match form {
                DeclarationForm::Data => kinds,
                DeclarationForm::Newtype { .. } => {
                    kinds && slots.len() == parameters.len() + 1 && slots.contains(&(12, 0))
                }
                _ => kinds && slots.len() == parameters.len(),
            }
        }
        TypeNode::ConstructorTemplate { constructor, .. } => {
            let physical = constructors
                .get(constructor.0 as usize)
                .ok_or(TypeGraphError::InvalidConstructor(index))?;
            if !contiguous(11, physical.field_reps.len())
                || slots.len() != physical.field_reps.len()
            {
                return Err(TypeGraphError::InvalidConstructor(index));
            }
            for edge in graph.edges(node) {
                budget.work(1)?;
                if let TypeEdge::Field {
                    ordinal,
                    source_rep,
                } = edge.weight()
                {
                    if physical.field_reps.get(*ordinal as usize) != Some(source_rep) {
                        return Err(TypeGraphError::InvalidConstructor(index));
                    }
                }
            }
            let mut incoming = graph.edges_directed(node, petgraph::Incoming);
            budget.work(2)?;
            let parent = incoming
                .next()
                .ok_or(TypeGraphError::InvalidConstructor(index))?;
            if incoming.next().is_some() {
                return Err(TypeGraphError::InvalidConstructor(index));
            }
            match (&graph[parent.source()], parent.weight()) {
                (
                    TypeNode::Declaration {
                        identity,
                        form: DeclarationForm::Data,
                        ..
                    },
                    TypeEdge::Constructor(tag),
                ) if budget.identity_eq(identity, &physical.family)?
                    && *tag == physical.tag
                    && *tag > 0 => {}
                _ => return Err(TypeGraphError::InvalidConstructor(index)),
            }
            true
        }
        TypeNode::NominalApplication => {
            slots.contains(&(2, 0)) && contiguous(3, slots.len().saturating_sub(1))
        }
        TypeNode::Application => exact(&[(4, 0), (5, 0)]),
        TypeNode::Function(_) => exact(&[(6, 0), (7, 0), (8, 0)]),
        TypeNode::ForAll(_) => exact(&[(1, 0), (9, 0)]),
        TypeNode::Bound(_) | TypeNode::Literal(_) => slots.is_empty(),
    };
    if !valid {
        return Err(TypeGraphError::InvalidCardinality(index));
    }
    if let TypeNode::Declaration {
        identity,
        form: DeclarationForm::Data,
        ..
    } = &graph[node]
    {
        let mut count = 0_usize;
        let expected = slots.iter().filter(|slot| slot.0 == 10).count();
        budget.work(symbol_work(identity))?;
        if families.get(identity).copied().unwrap_or(0) != expected {
            return Err(TypeGraphError::InvalidConstructor(index));
        }
        for edge in graph
            .edges(node)
            .filter(|edge| matches!(edge.weight(), TypeEdge::Constructor(_)))
        {
            budget.work(1)?;
            let TypeNode::ConstructorTemplate { constructor, .. } = &graph[edge.target()] else {
                return Err(TypeGraphError::InvalidConstructor(index));
            };
            let physical = constructors
                .get(constructor.0 as usize)
                .ok_or(TypeGraphError::InvalidConstructor(index))?;
            if physical.family_size as usize != expected || physical.tag as usize > expected {
                return Err(TypeGraphError::InvalidConstructor(index));
            }
            count += 1;
        }
        if !slots
            .iter()
            .filter(|slot| slot.0 == 10)
            .map(|slot| slot.1 as usize)
            .eq(1..=count)
        {
            return Err(TypeGraphError::InvalidConstructor(index));
        }
    }
    Ok(())
}

fn check_scope(
    graph: &GraphStorage,
    node: TypeNodeId,
    binders: usize,
    required: &[usize],
    budget: &mut Budget,
) -> Result<(), TypeGraphError> {
    for edge in graph.edges(node) {
        budget.work(1)?;
        let available = match edge.weight() {
            TypeEdge::BinderKind(position) => *position as usize,
            TypeEdge::Body | TypeEdge::Field { .. } => binders,
            _ => continue,
        };
        if required[edge.target().index()] > available {
            return Err(TypeGraphError::InvalidScope(node.index()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_schema::{CheckedLayout, FieldLayout, StorageLayout};
    use std::collections::hash_map::DefaultHasher;

    pub(super) fn identity(occurrence: &str, namespace: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: "fixture".into(),
            module: "FiniteTypes".into(),
            namespace: namespace.into(),
            occurrence: occurrence.into(),
            record_parent: None,
        }
    }
    fn root(
        graph: &mut GraphStorage,
        domain: RootDomain,
        binders: Vec<SourceBinderFlag>,
        rendered: &str,
    ) -> TypeNodeId {
        graph.add_node(TypeNode::Root {
            domain,
            binders,
            rendered: rendered.into(),
        })
    }
    fn kind(graph: &mut GraphStorage) -> TypeNodeId {
        graph.add_node(TypeNode::Literal(TypeLiteral::Symbol("kind".into())))
    }
    fn publish(graph: GraphStorage, constructors: &[ConstructorDecl]) -> TypeGraph {
        TypeGraph::validate(graph, constructors, GraphLimits::default()).unwrap()
    }
    pub(super) fn physical(fields: Vec<RuntimeRep>) -> ConstructorDecl {
        let target = crate::execution_schema::testing::target();
        let storage = StorageLayout::for_reps(&target, &fields).unwrap();
        ConstructorDecl {
            identity: identity("ReplyValue", "data"),
            family: identity("Reply", "type"),
            host_id: crate::DataConId(77),
            result_rep: RuntimeRep::LiftedRef,
            strict_fields: vec![false; fields.len()],
            layout: CheckedLayout {
                fields: storage
                    .fields()
                    .iter()
                    .map(|field| FieldLayout {
                        rep: field.rep(),
                        offset: field.offset(),
                    })
                    .collect(),
                alignment: storage.alignment(),
                payload_size: storage.payload_size(),
                root_mask: fields
                    .iter()
                    .map(|rep| matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef))
                    .collect(),
            },
            field_reps: fields,
            tag: 1,
            family_size: 1,
        }
    }

    /// The template calls its own declaration with its formal parameter. The
    /// same bound expression occupies two field slots; neither recursion nor
    /// sharing allocates a graph of instantiated types.
    fn recursive_graph(
        constructor: ConstructorId,
        inventory: &[ConstructorDecl],
        extra: bool,
    ) -> (GraphStorage, TypeNodeId, TypeNodeId) {
        let mut graph = GraphStorage::new();
        if extra {
            kind(&mut graph);
        }
        let root = root(&mut graph, RootDomain::Closed, vec![], "Reply 0");
        let declaration = graph.add_node(TypeNode::Declaration {
            identity: identity("Reply", "type"),
            parameters: vec![ParameterFlag::NamedRequired],
            form: DeclarationForm::Data,
            restriction: SyntaxRestriction::None,
        });
        let template = graph.add_node(TypeNode::ConstructorTemplate {
            constructor,
            identity: inventory[constructor.0 as usize].identity.clone(),
        });
        let kind = kind(&mut graph);
        let bound = graph.add_node(TypeNode::Bound(0));
        let field = graph.add_node(TypeNode::NominalApplication);
        let body = graph.add_node(TypeNode::NominalApplication);
        let argument = graph.add_node(TypeNode::Literal(TypeLiteral::Natural("0".into())));
        graph.add_edge(root, body, TypeEdge::Body);
        graph.add_edge(declaration, kind, TypeEdge::BinderKind(0));
        graph.add_edge(declaration, template, TypeEdge::Constructor(1));
        graph.add_edge(
            template,
            field,
            TypeEdge::Field {
                ordinal: 0,
                source_rep: RuntimeRep::LiftedRef,
            },
        );
        graph.add_edge(
            template,
            field,
            TypeEdge::Field {
                ordinal: 1,
                source_rep: RuntimeRep::LiftedRef,
            },
        );
        graph.add_edge(field, declaration, TypeEdge::Head);
        graph.add_edge(field, bound, TypeEdge::Argument(0));
        graph.add_edge(body, declaration, TypeEdge::Head);
        graph.add_edge(body, argument, TypeEdge::Argument(0));
        (graph, root, template)
    }

    fn hash(graph: &TypeGraph) -> u64 {
        let mut hash = DefaultHasher::new();
        graph.hash(&mut hash);
        hash.finish()
    }
    fn evidence(graph: &TypeGraph) -> Vec<u8> {
        let mut bytes = Vec::new();
        graph.write_evidence(|part| bytes.extend_from_slice(part));
        bytes
    }

    #[test]
    fn shared_bound_expression_is_validated_at_each_scope_boundary() {
        let mut graph = GraphStorage::new();
        let open = root(
            &mut graph,
            RootDomain::ConstructorScheme,
            vec![SourceBinderFlag::Specified],
            "a",
        );
        let kind = kind(&mut graph);
        let bound = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(open, kind, TypeEdge::BinderKind(0));
        graph.add_edge(open, bound, TypeEdge::Body);
        publish(graph.clone(), &[]);
        let closed = root(&mut graph, RootDomain::Closed, vec![], "a");
        graph.add_edge(closed, bound, TypeEdge::Body);
        assert_eq!(
            TypeGraph::validate(graph, &[], GraphLimits::default()),
            Err(TypeGraphError::InvalidScope(closed.index()))
        );
    }

    #[test]
    fn binder_kinds_reject_forward_references_and_forall_extends_only_body() {
        let mut graph = GraphStorage::new();
        let root = root(
            &mut graph,
            RootDomain::ConstructorScheme,
            vec![SourceBinderFlag::Specified, SourceBinderFlag::Inferred],
            "a",
        );
        let kind = kind(&mut graph);
        let bound = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(root, kind, TypeEdge::BinderKind(0));
        graph.add_edge(root, bound, TypeEdge::BinderKind(1));
        graph.add_edge(root, bound, TypeEdge::Body);
        publish(graph.clone(), &[]);
        let edge = graph
            .edges(root)
            .find(|edge| *edge.weight() == TypeEdge::BinderKind(0))
            .unwrap()
            .id();
        graph.remove_edge(edge);
        graph.add_edge(root, bound, TypeEdge::BinderKind(0));
        assert_eq!(
            TypeGraph::validate(graph, &[], GraphLimits::default()),
            Err(TypeGraphError::InvalidScope(root.index()))
        );

        let mut graph = GraphStorage::new();
        let root = self::root(&mut graph, RootDomain::Closed, vec![], "forall a. a");
        let forall = graph.add_node(TypeNode::ForAll(ForAllFlag::Specified));
        let kind = self::kind(&mut graph);
        let bound = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(root, forall, TypeEdge::Body);
        graph.add_edge(forall, kind, TypeEdge::Kind);
        graph.add_edge(forall, bound, TypeEdge::Body);
        publish(graph.clone(), &[]);
        let edge = graph
            .edges(forall)
            .find(|edge| *edge.weight() == TypeEdge::Kind)
            .unwrap()
            .id();
        graph.remove_edge(edge);
        graph.add_edge(forall, bound, TypeEdge::Kind);
        assert_eq!(
            TypeGraph::validate(graph, &[], GraphLimits::default()),
            Err(TypeGraphError::InvalidScope(root.index()))
        );
    }

    #[test]
    fn newtype_rhs_uses_eta_prefix_instead_of_full_parameter_telescope() {
        let mut graph = GraphStorage::new();
        let declaration = graph.add_node(TypeNode::Declaration {
            identity: identity("Eta", "type"),
            parameters: vec![
                ParameterFlag::NamedRequired,
                ParameterFlag::AnonymousVisible,
            ],
            form: DeclarationForm::Newtype { eta_arity: 1 },
            restriction: SyntaxRestriction::None,
        });
        let kind = kind(&mut graph);
        let prefix = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(declaration, kind, TypeEdge::BinderKind(0));
        graph.add_edge(declaration, kind, TypeEdge::BinderKind(1));
        graph.add_edge(declaration, prefix, TypeEdge::AliasRhs);
        publish(graph.clone(), &[]);
        *graph.node_weight_mut(prefix).unwrap() = TypeNode::Bound(1);
        assert_eq!(
            TypeGraph::validate(graph, &[], GraphLimits::default()),
            Err(TypeGraphError::InvalidScope(declaration.index()))
        );
    }

    #[test]
    fn recursive_templates_are_finite_but_expression_cycles_are_rejected() {
        let inventory = vec![physical(vec![RuntimeRep::LiftedRef; 2])];
        let (mut graph, root, template) = recursive_graph(ConstructorId(0), &inventory, false);
        let published = publish(graph.clone(), &inventory);
        assert!(published
            .rooted_identity_eq(root, &published, root, &mut TypeWorkBudget::new(10_000))
            .unwrap());
        assert_eq!(published.graph().node_count(), graph.node_count());
        let field = graph.edges(template).next().unwrap().target();
        let edge = graph
            .edges(field)
            .find(|edge| matches!(edge.weight(), TypeEdge::Argument(_)))
            .unwrap()
            .id();
        graph.remove_edge(edge);
        graph.add_edge(field, field, TypeEdge::Argument(0));
        assert_eq!(
            TypeGraph::validate(graph, &inventory, GraphLimits::default()),
            Err(TypeGraphError::ExpressionCycle)
        );
    }

    #[test]
    fn slots_ignore_field_rep_but_preserve_parallel_argument_ordinals() {
        let inventory = vec![physical(vec![RuntimeRep::LiftedRef; 2])];
        let (mut graph, _, template) = recursive_graph(ConstructorId(0), &inventory, false);
        let field = graph.edges(template).next().unwrap().target();
        graph.add_edge(
            template,
            field,
            TypeEdge::Field {
                ordinal: 0,
                source_rep: RuntimeRep::Int(64),
            },
        );
        assert_eq!(
            TypeGraph::validate(graph, &inventory, GraphLimits::default()),
            Err(TypeGraphError::InvalidCardinality(template.index()))
        );

        let mut graph = GraphStorage::new();
        let root = root(&mut graph, RootDomain::Closed, vec![], "Pair 0 0");
        let declaration = graph.add_node(TypeNode::Declaration {
            identity: identity("Pair", "type"),
            parameters: vec![ParameterFlag::NamedRequired; 2],
            form: DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "fixture".into(),
            },
            restriction: SyntaxRestriction::None,
        });
        let kind = kind(&mut graph);
        graph.add_edge(declaration, kind, TypeEdge::BinderKind(0));
        graph.add_edge(declaration, kind, TypeEdge::BinderKind(1));
        let application = graph.add_node(TypeNode::NominalApplication);
        let argument = graph.add_node(TypeNode::Literal(TypeLiteral::Natural("0".into())));
        graph.add_edge(root, application, TypeEdge::Body);
        graph.add_edge(application, declaration, TypeEdge::Head);
        graph.add_edge(application, argument, TypeEdge::Argument(0));
        graph.add_edge(application, argument, TypeEdge::Argument(1));
        let graph = publish(graph, &[]);
        assert_eq!(
            graph
                .ordered_edges(application)
                .map(|edge| *edge.weight())
                .collect::<Vec<_>>(),
            vec![TypeEdge::Head, TypeEdge::Argument(0), TypeEdge::Argument(1)]
        );
        assert_eq!(
            graph
                .graph()
                .edges_connecting(application, argument)
                .count(),
            2
        );
    }

    #[test]
    fn physical_pairing_and_nominal_declaration_collisions_refuse() {
        let inventory = vec![physical(vec![RuntimeRep::LiftedRef; 2])];
        let (graph, _, template) = recursive_graph(ConstructorId(0), &inventory, false);
        let graph = publish(graph, &inventory);
        let mut changed = inventory.clone();
        changed[0].field_reps[0] = RuntimeRep::Int(64);
        assert_eq!(
            graph.check_constructor_pairing(&changed),
            Err(TypeGraphError::InvalidConstructor(template.index()))
        );
        let mut changed = inventory.clone();
        changed[0].family = identity("Other", "type");
        assert_eq!(
            graph.check_constructor_pairing(&changed),
            Err(TypeGraphError::InvalidConstructor(template.index()))
        );
        let mut input = graph.graph().clone();
        input.add_node(TypeNode::Declaration {
            identity: identity("Reply", "type"),
            parameters: vec![],
            form: DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "different".into(),
            },
            restriction: SyntaxRestriction::None,
        });
        assert!(matches!(
            TypeGraph::validate(input, &inventory, GraphLimits::default()),
            Err(TypeGraphError::DuplicateDeclaration(_))
        ));
    }

    #[test]
    fn removing_all_templates_cannot_turn_a_physical_family_into_an_empty_type() {
        let inventory = vec![physical(vec![])];
        let mut complete = GraphStorage::new();
        let declaration = complete.add_node(TypeNode::Declaration {
            identity: identity("Reply", "type"),
            parameters: vec![],
            form: DeclarationForm::Data,
            restriction: SyntaxRestriction::None,
        });
        let template = complete.add_node(TypeNode::ConstructorTemplate {
            constructor: ConstructorId(0),
            identity: inventory[0].identity.clone(),
        });
        complete.add_edge(declaration, template, TypeEdge::Constructor(1));
        publish(complete.clone(), &inventory);
        complete.remove_node(template);
        assert_eq!(
            TypeGraph::validate(complete, &inventory, GraphLimits::default()),
            Err(TypeGraphError::InvalidConstructor(declaration.index()))
        );
        let mut empty = GraphStorage::new();
        let declaration = empty.add_node(TypeNode::Declaration {
            identity: identity("Reply", "type"),
            parameters: vec![],
            form: DeclarationForm::Data,
            restriction: SyntaxRestriction::None,
        });
        let empty = publish(empty, &[]);
        assert_eq!(
            empty.check_constructor_pairing(&inventory),
            Err(TypeGraphError::InvalidConstructor(declaration.index()))
        );
        empty.check_constructor_pairing(&[]).unwrap();
    }

    #[test]
    fn identical_physical_layout_does_not_hide_template_kind_or_restriction_changes() {
        let inventory = vec![physical(vec![RuntimeRep::LiftedRef])];
        let mut input = GraphStorage::new();
        let root = root(&mut input, RootDomain::Closed, vec![], "Reply 0 1");
        let declaration = input.add_node(TypeNode::Declaration {
            identity: identity("Reply", "type"),
            parameters: vec![ParameterFlag::NamedRequired; 2],
            form: DeclarationForm::Data,
            restriction: SyntaxRestriction::None,
        });
        let template = input.add_node(TypeNode::ConstructorTemplate {
            constructor: ConstructorId(0),
            identity: inventory[0].identity.clone(),
        });
        let kind = kind(&mut input);
        let field = input.add_node(TypeNode::Bound(0));
        let body = input.add_node(TypeNode::NominalApplication);
        let zero = input.add_node(TypeNode::Literal(TypeLiteral::Natural("0".into())));
        let one = input.add_node(TypeNode::Literal(TypeLiteral::Natural("1".into())));
        input.add_edge(root, body, TypeEdge::Body);
        input.add_edge(body, declaration, TypeEdge::Head);
        input.add_edge(body, zero, TypeEdge::Argument(0));
        input.add_edge(body, one, TypeEdge::Argument(1));
        input.add_edge(declaration, kind, TypeEdge::BinderKind(0));
        input.add_edge(declaration, kind, TypeEdge::BinderKind(1));
        input.add_edge(declaration, template, TypeEdge::Constructor(1));
        input.add_edge(
            template,
            field,
            TypeEdge::Field {
                ordinal: 0,
                source_rep: RuntimeRep::LiftedRef,
            },
        );
        let original = publish(input.clone(), &inventory);
        for change in 0..3 {
            let mut changed = input.clone();
            match change {
                0 => *changed.node_weight_mut(field).unwrap() = TypeNode::Bound(1),
                1 => {
                    *changed.node_weight_mut(kind).unwrap() =
                        TypeNode::Literal(TypeLiteral::Symbol("other kind".into()))
                }
                _ => {
                    if let TypeNode::Declaration { restriction, .. } =
                        changed.node_weight_mut(declaration).unwrap()
                    {
                        *restriction = SyntaxRestriction::EffectHead;
                    }
                }
            }
            let changed = publish(changed, &inventory);
            changed.check_constructor_pairing(&inventory).unwrap();
            assert!(!original
                .declaration_identity_eq(
                    declaration,
                    &changed,
                    declaration,
                    &mut TypeWorkBudget::new(10_000)
                )
                .unwrap());
            assert!(!original
                .rooted_identity_eq(root, &changed, root, &mut TypeWorkBudget::new(10_000))
                .unwrap());
        }
    }

    #[test]
    fn reachable_identity_remaps_storage_and_physical_ids_but_evidence_keeps_tables() {
        let physical = physical(vec![RuntimeRep::LiftedRef; 2]);
        let first_inventory = vec![physical.clone()];
        let (first, first_root, _) = recursive_graph(ConstructorId(0), &first_inventory, false);
        let mut unrelated = physical.clone();
        unrelated.identity = identity("Unrelated", "data");
        unrelated.family = identity("UnrelatedFamily", "type");
        unrelated.host_id = crate::DataConId(78);
        let second_inventory = vec![unrelated, physical];
        let (second, second_root, _) = recursive_graph(ConstructorId(1), &second_inventory, true);
        let first = publish(first, &first_inventory);
        let second = publish(second, &second_inventory);
        assert!(first
            .rooted_identity_eq(
                first_root,
                &second,
                second_root,
                &mut TypeWorkBudget::new(10_000)
            )
            .unwrap());
        assert!(!first.evidence_eq(&second));
        assert_ne!(evidence(&first), evidence(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn diagnostics_only_change_exact_content_and_not_scoped_evidence() {
        let mut first = GraphStorage::new();
        let root = root(&mut first, RootDomain::Closed, vec![], "F Int");
        let declaration = first.add_node(TypeNode::Declaration {
            identity: identity("F", "type"),
            parameters: vec![],
            form: DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Family,
                reason: "type family".into(),
            },
            restriction: SyntaxRestriction::None,
        });
        let body = first.add_node(TypeNode::NominalApplication);
        first.add_edge(root, body, TypeEdge::Body);
        first.add_edge(body, declaration, TypeEdge::Head);
        let mut second = first.clone();
        if let TypeNode::Root { rendered, .. } = second.node_weight_mut(root).unwrap() {
            *rendered = "F result1".into();
        }
        if let TypeNode::Declaration {
            form: DeclarationForm::Opaque { reason, .. },
            ..
        } = second.node_weight_mut(declaration).unwrap()
        {
            *reason = "different diagnostic".into();
        }
        let first = publish(first, &[]);
        let second = publish(second, &[]);
        assert!(first.evidence_eq(&second));
        assert_eq!(evidence(&first), evidence(&second));
        assert_ne!(first, second);
        assert_ne!(hash(&first), hash(&second));
        assert!(first
            .rooted_identity_eq(root, &second, root, &mut TypeWorkBudget::new(10_000))
            .unwrap());
        let mut changed = second.graph().clone();
        if let TypeNode::Declaration {
            form: DeclarationForm::Opaque { head_kind, .. },
            ..
        } = changed.node_weight_mut(declaration).unwrap()
        {
            *head_kind = NominalHeadKind::Constructor;
        }
        let changed = publish(changed, &[]);
        assert!(!second.evidence_eq(&changed));
        assert!(!second
            .rooted_identity_eq(root, &changed, root, &mut TypeWorkBudget::new(10_000))
            .unwrap());
    }

    #[test]
    fn freeze_normalizes_adjacency_once_and_validation_work_is_returned() {
        let inventory = vec![physical(vec![RuntimeRep::LiftedRef; 2])];
        let (first, _, _) = recursive_graph(ConstructorId(0), &inventory, false);
        let same_input = first.clone();
        let mut second = first.clone();
        let edges: Vec<_> = second
            .edge_references()
            .map(|edge| (edge.source(), edge.target(), *edge.weight()))
            .collect();
        second.clear_edges();
        for (source, target, role) in edges.into_iter().rev() {
            second.add_edge(source, target, role);
        }
        let (first, work) =
            TypeGraph::validate_with_work(first, &inventory, GraphLimits::default()).unwrap();
        let second = publish(second, &inventory);
        assert_eq!(first, second);
        assert_eq!(hash(&first), hash(&second));
        assert!(work > first.graph().node_count() + first.graph().edge_count());
        let limits = GraphLimits {
            max_work: work - 1,
            ..GraphLimits::default()
        };
        assert_eq!(
            TypeGraph::validate(same_input, &inventory, limits),
            Err(TypeGraphError::Limit("work"))
        );
        let mut budget = TypeWorkBudget::new(0);
        let root = first
            .graph()
            .node_indices()
            .find(|node| matches!(first.graph()[*node], TypeNode::Root { .. }))
            .unwrap();
        assert_eq!(
            first.rooted_identity_eq(root, &first, root, &mut budget),
            Err(TypeGraphError::TraversalWork)
        );
    }
}
