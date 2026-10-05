//! Finite compiler type declarations and scoped expressions.
//!
//! Node weights retain metadata; all child relationships live in the graph.
//! Original compiler admission and physical constructor authority remain with
//! their existing owners. A graph describes type evidence, not authority.

use petgraph::graph::{Edges, Graph, NodeIndex};
use petgraph::Directed;

use crate::execution_schema::{ConstructorId, RuntimeRep, SymbolIdentity};

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
    AnonymousInvisible,
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
    pub fn graph(&self) -> &GraphStorage {
        &self.graph
    }

    /// Borrowed ascending role/ordinal iteration. Publication fixes insertion
    /// order once; reads neither sort nor create an adjacency representation.
    pub fn ordered_edges(&self, node: TypeNodeId) -> Edges<'_, TypeEdge, Directed, u32> {
        self.graph.edges(node)
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
        Self {
            max_nodes: 65_535,
            max_edges: 1 << 24,
            max_bytes: 16 << 20,
            max_work: 1 << 24,
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
    #[error("finite type graph has an expression cycle")]
    ExpressionCycle,
    #[error("finite type graph has an out-of-scope variable at node {0}")]
    InvalidScope(usize),
    #[error("finite type graph node {0} is not a root")]
    NotRoot(usize),
    #[error("finite type graph comparison exceeds its work budget")]
    ComparisonWork,
}
