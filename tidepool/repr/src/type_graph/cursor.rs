//! Demand views over the one frozen graph. Environments share argument
//! closures; evaluation never substitutes or materializes expression syntax.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use petgraph::visit::EdgeRef;

use super::{
    evidence_metadata_work, DeclarationForm, SyntaxRestriction, TypeEdge, TypeGraph,
    TypeGraphError, TypeNode, TypeNodeId, TypeWorkBudget,
};
use crate::execution_schema::{
    ConstructorId, RuntimeRep, SymbolIdentity, TypeNodeId as WireTypeNodeId,
};

#[derive(Clone, Default)]
struct Environment(Option<Arc<Frame>>);

struct Frame {
    /// Newest binder first. Alias/declaration frames start a fresh scope;
    /// ForAll frames share the outer scope through `parent`.
    bindings: Box<[Binding]>,
    parent: Environment,
}

#[derive(Clone)]
enum Binding {
    Argument(Closure),
    Unknown(u64),
}

#[derive(Clone)]
struct Closure {
    expression: TypeNodeId,
    environment: Environment,
    /// Arguments reapplied after an eta-reduced newtype RHS. These are shared
    /// closures, not new Application nodes or copied type syntax.
    applied: Option<Arc<ApplicationArguments>>,
}

struct ApplicationArguments(Box<[Closure]>);

impl Closure {
    fn new(expression: TypeNodeId, environment: Environment) -> Self {
        Self {
            expression,
            environment,
            applied: None,
        }
    }
}

impl Environment {
    fn arguments(
        arguments: &[Closure],
        budget: &mut TypeWorkBudget,
    ) -> Result<Self, TypeGraphError> {
        budget.charge(arguments.len())?;
        if arguments.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self(Some(Arc::new(Frame {
            bindings: arguments
                .iter()
                .rev()
                .cloned()
                .map(Binding::Argument)
                .collect(),
            parent: Self::default(),
        }))))
    }

    fn unknowns(count: usize, budget: &mut TypeWorkBudget) -> Result<Self, TypeGraphError> {
        budget.charge(count)?;
        if count == 0 {
            return Ok(Self::default());
        }
        Ok(Self(Some(Arc::new(Frame {
            bindings: (0..count)
                .rev()
                .map(|ordinal| Binding::Unknown(ordinal as u64))
                .collect(),
            parent: Self::default(),
        }))))
    }

    fn extend_unknown(
        &self,
        symbol: u64,
        budget: &mut TypeWorkBudget,
    ) -> Result<Self, TypeGraphError> {
        budget.charge(1)?;
        Ok(Self(Some(Arc::new(Frame {
            bindings: vec![Binding::Unknown(symbol)].into_boxed_slice(),
            parent: self.clone(),
        }))))
    }

    fn binding(
        &self,
        mut index: usize,
        budget: &mut TypeWorkBudget,
    ) -> Result<Binding, TypeGraphError> {
        let mut environment = self;
        while let Some(frame) = &environment.0 {
            budget.charge(1)?;
            if let Some(binding) = frame.bindings.get(index) {
                return Ok(binding.clone());
            }
            index -= frame.bindings.len();
            environment = &frame.parent;
        }
        Err(TypeGraphError::InvalidScope(index))
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        // Argument closures can retain a long chain of older frames. Consume
        // uniquely owned frames on a heap work stack instead of recursively
        // dropping that chain when a bounded operation finishes or refuses.
        let Some(frame) = self.0.take() else {
            return;
        };
        release(vec![Retained::Frame(frame)]);
    }
}

impl Drop for Closure {
    fn drop(&mut self) {
        if let Some(arguments) = self.applied.take() {
            release(vec![Retained::Application(arguments)]);
        }
    }
}

enum Retained {
    Frame(Arc<Frame>),
    Application(Arc<ApplicationArguments>),
}

fn retain_closure(closure: &mut Closure, pending: &mut Vec<Retained>) {
    if let Some(frame) = closure.environment.0.take() {
        pending.push(Retained::Frame(frame));
    }
    if let Some(arguments) = closure.applied.take() {
        pending.push(Retained::Application(arguments));
    }
}

fn release(mut pending: Vec<Retained>) {
    while let Some(retained) = pending.pop() {
        match retained {
            Retained::Frame(frame) => {
                let Some(Frame {
                    bindings,
                    mut parent,
                }) = Arc::into_inner(frame)
                else {
                    continue;
                };
                if let Some(parent) = parent.0.take() {
                    pending.push(Retained::Frame(parent));
                }
                for binding in bindings.into_vec() {
                    if let Binding::Argument(mut argument) = binding {
                        retain_closure(&mut argument, &mut pending);
                    }
                }
            }
            Retained::Application(arguments) => {
                let Some(ApplicationArguments(arguments)) = Arc::into_inner(arguments) else {
                    continue;
                };
                for mut argument in arguments.into_vec() {
                    retain_closure(&mut argument, &mut pending);
                }
            }
        }
    }
}

/// A scoped expression with its immutable graph owner. Cloning retains shared
/// environments, including repeated arguments, without traversing them.
#[derive(Clone)]
pub struct TypeCursor {
    owner: Arc<TypeGraph>,
    closure: Closure,
    root: TypeNodeId,
}

impl fmt::Debug for TypeCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeCursor")
            .field("expression", &self.expression())
            .field("rendered", &self.rendered())
            .finish_non_exhaustive()
    }
}

impl TypeCursor {
    pub fn owner(&self) -> &Arc<TypeGraph> {
        &self.owner
    }
    pub fn expression(&self) -> TypeNodeId {
        self.closure.expression
    }
    pub fn rendered(&self) -> &str {
        match &self.owner.graph()[self.root] {
            TypeNode::Root { rendered, .. } => rendered,
            _ => unreachable!("a cursor retains its validated source root"),
        }
    }

    pub fn view(&self, budget: &mut TypeWorkBudget) -> Result<TypeView, TypeGraphError> {
        let Some(head) = weak_head(&self.owner, self.closure.clone(), budget)? else {
            return Ok(TypeView::Unconstructible(
                ConstructionRefusal::RecursiveNewtype,
            ));
        };
        Ok(match classify(&self.owner, &head, budget)? {
            HeadClass::Data => TypeView::Data(DataView {
                owner: self.owner.clone(),
                declaration: head
                    .nominal()
                    .ok_or(TypeGraphError::InvalidRole(self.expression().index()))?,
                arguments: head.arguments,
                root: self.root,
            }),
            HeadClass::Text => TypeView::Text,
            HeadClass::Integer => TypeView::Integer,
            HeadClass::Natural => TypeView::Natural,
            HeadClass::Scalar(rep) => TypeView::Scalar(rep),
            HeadClass::Effectful => TypeView::Unconstructible(ConstructionRefusal::Effectful),
            HeadClass::Unsaturated { expected, actual } => {
                TypeView::Unconstructible(ConstructionRefusal::Unsaturated { expected, actual })
            }
            HeadClass::Function => TypeView::Unconstructible(ConstructionRefusal::Function),
            HeadClass::Polymorphic => TypeView::Unconstructible(ConstructionRefusal::Polymorphic),
            HeadClass::Unnormalized => TypeView::Unconstructible(ConstructionRefusal::Unnormalized),
            HeadClass::Declared(declaration) => {
                TypeView::Unconstructible(ConstructionRefusal::Declared {
                    owner: self.owner.clone(),
                    declaration,
                })
            }
        })
    }
}

#[derive(Clone, Debug)]
pub enum TypeView {
    Data(DataView),
    Text,
    Integer,
    Natural,
    Scalar(RuntimeRep),
    Unconstructible(ConstructionRefusal),
}

/// A refusal is construction policy, never a substitute for type identity.
#[derive(Clone)]
pub enum ConstructionRefusal {
    Effectful,
    RecursiveNewtype,
    Unsaturated {
        expected: usize,
        actual: usize,
    },
    Function,
    Polymorphic,
    Unnormalized,
    Declared {
        owner: Arc<TypeGraph>,
        declaration: TypeNodeId,
    },
}

impl fmt::Display for ConstructionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Effectful => f.write_str("effectful"),
            Self::RecursiveNewtype => f.write_str("recursive newtype"),
            Self::Unsaturated { expected, actual } => write!(
                f,
                "type argument arity mismatch: expected {expected}, got {actual}"
            ),
            Self::Function => f.write_str("function"),
            Self::Polymorphic => f.write_str("polymorphic"),
            Self::Unnormalized => f.write_str("unnormalized"),
            Self::Declared { owner, declaration } => {
                match owner.graph().node_weight(*declaration) {
                    Some(TypeNode::Declaration {
                        form: DeclarationForm::Opaque { reason, .. },
                        ..
                    }) => f.write_str(reason),
                    _ => f.write_str("unnormalized"),
                }
            }
        }
    }
}

impl fmt::Debug for ConstructionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A normalized data head. Field closures are created only for the selected
/// original constructor and share one formal-parameter environment.
#[derive(Clone)]
pub struct DataView {
    owner: Arc<TypeGraph>,
    declaration: TypeNodeId,
    arguments: Arc<[Closure]>,
    root: TypeNodeId,
}

impl fmt::Debug for DataView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DataView")
            .field("family", self.family())
            .field("argument_count", &self.argument_count())
            .finish_non_exhaustive()
    }
}

impl DataView {
    pub fn family(&self) -> &SymbolIdentity {
        match &self.owner.graph()[self.declaration] {
            TypeNode::Declaration { identity, .. } => identity,
            _ => unreachable!("a data view retains a validated declaration"),
        }
    }
    pub fn argument_count(&self) -> usize {
        self.arguments.len()
    }
    pub fn constructors(&self) -> impl Iterator<Item = ConstructorId> + '_ {
        self.owner
            .ordered_edges(self.declaration)
            .filter_map(|edge| {
                if !matches!(edge.weight(), TypeEdge::Constructor(_)) {
                    return None;
                }
                match &self.owner.graph()[edge.target()] {
                    TypeNode::ConstructorTemplate { constructor, .. } => Some(*constructor),
                    _ => None,
                }
            })
    }
    pub fn fields(
        &self,
        constructor: ConstructorId,
        budget: &mut TypeWorkBudget,
    ) -> Result<Option<Vec<TypeCursor>>, TypeGraphError> {
        for edge in self.owner.ordered_edges(self.declaration) {
            budget.charge(1)?;
            if !matches!(edge.weight(), TypeEdge::Constructor(_)) {
                continue;
            }
            let template = edge.target();
            let TypeNode::ConstructorTemplate {
                constructor: candidate,
                ..
            } = &self.owner.graph()[template]
            else {
                return Err(TypeGraphError::InvalidConstructor(template.index()));
            };
            if *candidate != constructor {
                continue;
            }
            let environment = Environment::arguments(&self.arguments, budget)?;
            let mut fields = Vec::new();
            for field in self.owner.ordered_edges(template) {
                budget.charge(1)?;
                fields.push(TypeCursor {
                    owner: self.owner.clone(),
                    closure: Closure::new(field.target(), environment.clone()),
                    root: self.root,
                });
            }
            return Ok(Some(fields));
        }
        Ok(None)
    }
}

impl TypeGraph {
    pub fn open_root(
        self: &Arc<Self>,
        root: WireTypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<TypeCursor, TypeGraphError> {
        let root = TypeNodeId::new(root.0 as usize);
        budget.charge(1)?;
        let Some(TypeNode::Root { binders, .. }) = self.graph().node_weight(root) else {
            return Err(TypeGraphError::NotRoot(root.index()));
        };
        let environment = Environment::unknowns(binders.len(), budget)?;
        let expression = child(self, root, TypeEdge::Body, budget)?;
        Ok(TypeCursor {
            owner: self.clone(),
            closure: Closure::new(expression, environment),
            root,
        })
    }

    /// Exact scoped identity after validating both serialized root references.
    pub fn rooted_wire_identity_eq(
        self: &Arc<Self>,
        root: WireTypeNodeId,
        other: &Arc<Self>,
        other_root: WireTypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        let first = self.open_root(root, budget)?;
        let second = other.open_root(other_root, budget)?;
        self.rooted_identity_eq(first.root, other, second.root, budget)
    }

    pub fn rooted_compatible(
        self: &Arc<Self>,
        root: WireTypeNodeId,
        other: &Arc<Self>,
        other_root: WireTypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        let first = self.open_root(root, budget)?;
        let second = other.open_root(other_root, budget)?;
        let (
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
        ) = (&self.graph()[first.root], &other.graph()[second.root])
        else {
            return Err(TypeGraphError::NotRoot(first.root.index()));
        };
        budget.charge(first_binders.len().max(second_binders.len()))?;
        if first_domain != second_domain || first_binders != second_binders {
            return Ok(false);
        }
        let mut first_kinds = self
            .ordered_edges(first.root)
            .filter(|edge| matches!(edge.weight(), TypeEdge::BinderKind(_)));
        let mut second_kinds = other
            .ordered_edges(second.root)
            .filter(|edge| matches!(edge.weight(), TypeEdge::BinderKind(_)));
        loop {
            budget.charge(1)?;
            match (first_kinds.next(), second_kinds.next()) {
                (None, None) => break,
                (Some(first), Some(second)) if first.weight() == second.weight() => {
                    if !self.node_identity_eq(first.target(), other, second.target(), budget)? {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
        compatible(self, first.closure, other, second.closure, budget)
    }
}

fn child(
    graph: &TypeGraph,
    node: TypeNodeId,
    role: TypeEdge,
    budget: &mut TypeWorkBudget,
) -> Result<TypeNodeId, TypeGraphError> {
    for edge in graph.ordered_edges(node) {
        budget.charge(1)?;
        if *edge.weight() == role {
            return Ok(edge.target());
        }
    }
    Err(TypeGraphError::InvalidRole(node.index()))
}

enum HeadKind {
    Nominal(TypeNodeId),
    Unknown(u64),
    Atomic(Closure),
}
struct Head {
    kind: HeadKind,
    arguments: Arc<[Closure]>,
}
impl Head {
    fn nominal(&self) -> Option<TypeNodeId> {
        match self.kind {
            HeadKind::Nominal(declaration) => Some(declaration),
            _ => None,
        }
    }
}

/// Resolve lexical variables and application spines, without unfolding any
/// nominal declaration. Arguments remain closures in their original scope.
fn resolve_head(
    graph: &TypeGraph,
    mut closure: Closure,
    budget: &mut TypeWorkBudget,
) -> Result<Head, TypeGraphError> {
    let mut pending = Vec::new();
    loop {
        budget.charge(1)?;
        if let Some(applied) = closure.applied.take() {
            budget.charge(applied.0.len())?;
            pending.extend(applied.0.iter().rev().cloned());
        }
        match graph
            .graph()
            .node_weight(closure.expression)
            .ok_or(TypeGraphError::InvalidReference(closure.expression.index()))?
        {
            TypeNode::Bound(index) => match closure.environment.binding(*index as usize, budget)? {
                Binding::Argument(argument) => closure = argument,
                Binding::Unknown(symbol) => {
                    return Ok(Head {
                        kind: HeadKind::Unknown(symbol),
                        arguments: pending.into_iter().rev().collect::<Vec<_>>().into(),
                    })
                }
            },
            TypeNode::Application => {
                let argument = child(graph, closure.expression, TypeEdge::ApplyArgument, budget)?;
                pending.push(Closure::new(argument, closure.environment.clone()));
                closure.expression = child(graph, closure.expression, TypeEdge::Function, budget)?;
            }
            TypeNode::NominalApplication => {
                let mut declaration = None;
                let mut arguments = Vec::new();
                for edge in graph.ordered_edges(closure.expression) {
                    budget.charge(1)?;
                    match edge.weight() {
                        TypeEdge::Head => declaration = Some(edge.target()),
                        TypeEdge::Argument(_) => {
                            arguments.push(Closure::new(edge.target(), closure.environment.clone()))
                        }
                        _ => return Err(TypeGraphError::InvalidRole(closure.expression.index())),
                    }
                }
                arguments.extend(pending.into_iter().rev());
                return Ok(Head {
                    kind: HeadKind::Nominal(
                        declaration
                            .ok_or(TypeGraphError::InvalidRole(closure.expression.index()))?,
                    ),
                    arguments: arguments.into(),
                });
            }
            node if node.is_expression() => {
                return Ok(Head {
                    kind: HeadKind::Atomic(closure),
                    arguments: pending.into_iter().rev().collect::<Vec<_>>().into(),
                })
            }
            _ => return Err(TypeGraphError::InvalidRole(closure.expression.index())),
        }
    }
}

/// The only head-newtype evaluator, shared by construction and compatibility.
/// A repeated instantiated alias is a cycle; parameter-changing chains remain
/// lazy and stop through the caller's one work budget if they never reach data.
fn weak_head(
    graph: &TypeGraph,
    mut closure: Closure,
    budget: &mut TypeWorkBudget,
) -> Result<Option<Head>, TypeGraphError> {
    let mut active: HashMap<TypeNodeId, Vec<Arc<[Closure]>>> = HashMap::new();
    let mut syntax = SyntaxComparer::new(graph, graph);
    loop {
        let head = resolve_head(graph, closure, budget)?;
        let HeadKind::Nominal(declaration) = head.kind else {
            return Ok(Some(head));
        };
        let TypeNode::Declaration {
            form: DeclarationForm::Newtype { eta_arity },
            ..
        } = &graph.graph()[declaration]
        else {
            return Ok(Some(head));
        };
        let arity = *eta_arity as usize;
        if head.arguments.len() < arity {
            return Ok(Some(head));
        }
        if let Some(previous) = active.get(&declaration) {
            for arguments in previous {
                budget.charge(1)?;
                if syntax.arguments_equal(arguments, &head.arguments, budget)? {
                    return Ok(None);
                }
            }
        }
        budget.charge(1)?;
        active
            .entry(declaration)
            .or_default()
            .push(head.arguments.clone());
        let environment = Environment::arguments(&head.arguments[..arity], budget)?;
        let expression = child(graph, declaration, TypeEdge::AliasRhs, budget)?;
        budget.charge(head.arguments.len() - arity)?;
        let trailing = &head.arguments[arity..];
        closure = Closure {
            expression,
            environment,
            applied: if trailing.is_empty() {
                None
            } else {
                Some(Arc::new(ApplicationArguments(
                    trailing.to_vec().into_boxed_slice(),
                )))
            },
        };
    }
}

#[derive(Clone, Copy)]
enum HeadClass {
    Data,
    Text,
    Integer,
    Natural,
    Scalar(RuntimeRep),
    Effectful,
    Unsaturated { expected: usize, actual: usize },
    Function,
    Polymorphic,
    Unnormalized,
    Declared(TypeNodeId),
}

fn classify(
    graph: &TypeGraph,
    head: &Head,
    budget: &mut TypeWorkBudget,
) -> Result<HeadClass, TypeGraphError> {
    if contains_effect(graph, head, budget)? {
        return Ok(HeadClass::Effectful);
    }
    Ok(match &head.kind {
        HeadKind::Nominal(declaration) => {
            let TypeNode::Declaration {
                parameters, form, ..
            } = &graph.graph()[*declaration]
            else {
                return Err(TypeGraphError::InvalidRole(declaration.index()));
            };
            if head.arguments.len() != parameters.len() {
                HeadClass::Unsaturated {
                    expected: parameters.len(),
                    actual: head.arguments.len(),
                }
            } else {
                match form {
                    DeclarationForm::Data => HeadClass::Data,
                    DeclarationForm::Text => HeadClass::Text,
                    DeclarationForm::Integer => HeadClass::Integer,
                    DeclarationForm::Natural => HeadClass::Natural,
                    DeclarationForm::Scalar(rep) => HeadClass::Scalar(*rep),
                    DeclarationForm::Newtype { .. } => HeadClass::Unnormalized,
                    DeclarationForm::Opaque { .. } => HeadClass::Declared(*declaration),
                }
            }
        }
        HeadKind::Unknown(_) => HeadClass::Polymorphic,
        HeadKind::Atomic(closure) => match &graph.graph()[closure.expression] {
            TypeNode::Function(_) if head.arguments.is_empty() => HeadClass::Function,
            TypeNode::ForAll(_) if head.arguments.is_empty() => HeadClass::Polymorphic,
            _ => HeadClass::Unnormalized,
        },
    })
}

/// Scan syntax and substituted arguments, without recursively classifying them
/// or following declaration fields, binder metadata, or nested alias RHSs.
fn contains_effect(
    graph: &TypeGraph,
    head: &Head,
    budget: &mut TypeWorkBudget,
) -> Result<bool, TypeGraphError> {
    let mut pending = Vec::new();
    budget.charge(head.arguments.len())?;
    pending.extend(head.arguments.iter().cloned());
    match &head.kind {
        HeadKind::Nominal(declaration) => {
            if effect_head(graph, *declaration) {
                return Ok(true);
            }
        }
        HeadKind::Atomic(closure) => pending.push(closure.clone()),
        HeadKind::Unknown(_) => {}
    }
    let mut visited = HashSet::new();
    while let Some(closure) = pending.pop() {
        budget.charge(1)?;
        if !visited.insert(ScopedKey::new(&closure)) {
            continue;
        }
        let head = resolve_head(graph, closure, budget)?;
        budget.charge(head.arguments.len())?;
        pending.extend(head.arguments.iter().cloned());
        match head.kind {
            HeadKind::Nominal(declaration) => {
                if effect_head(graph, declaration) {
                    return Ok(true);
                }
            }
            HeadKind::Unknown(_) => {}
            HeadKind::Atomic(closure) => match &graph.graph()[closure.expression] {
                TypeNode::ForAll(_) => {
                    let kind = child(graph, closure.expression, TypeEdge::Kind, budget)?;
                    let body = child(graph, closure.expression, TypeEdge::Body, budget)?;
                    pending.push(Closure::new(kind, closure.environment.clone()));
                    pending.push(Closure::new(
                        body,
                        closure.environment.extend_unknown(u64::MAX, budget)?,
                    ));
                }
                _ => {
                    for edge in graph.ordered_edges(closure.expression) {
                        budget.charge(1)?;
                        pending.push(Closure::new(edge.target(), closure.environment.clone()));
                    }
                }
            },
        }
    }
    Ok(false)
}

fn effect_head(graph: &TypeGraph, declaration: TypeNodeId) -> bool {
    matches!(
        graph.graph()[declaration],
        TypeNode::Declaration {
            restriction: SyntaxRestriction::EffectHead,
            ..
        }
    )
}

/// Pointer-based keys retain their Arc guards. No memo can outlive an owner or
/// observe a recycled environment/application address as the same scope.
#[derive(Clone)]
struct ScopedKey {
    expression: TypeNodeId,
    environment: Environment,
    applied: Option<Arc<ApplicationArguments>>,
}
impl ScopedKey {
    fn new(closure: &Closure) -> Self {
        Self {
            expression: closure.expression,
            environment: closure.environment.clone(),
            applied: closure.applied.clone(),
        }
    }
}
impl PartialEq for ScopedKey {
    fn eq(&self, other: &Self) -> bool {
        self.expression == other.expression
            && match (&self.environment.0, &other.environment.0) {
                (None, None) => true,
                (Some(first), Some(second)) => Arc::ptr_eq(first, second),
                _ => false,
            }
            && match (&self.applied, &other.applied) {
                (None, None) => true,
                (Some(first), Some(second)) => Arc::ptr_eq(first, second),
                _ => false,
            }
    }
}
impl Eq for ScopedKey {}
impl Hash for ScopedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.expression.hash(state);
        self.environment
            .0
            .as_ref()
            .map(|frame| Arc::as_ptr(frame) as usize)
            .hash(state);
        self.applied
            .as_ref()
            .map(|arguments| Arc::as_ptr(arguments) as usize)
            .hash(state);
    }
}

impl Drop for ScopedKey {
    fn drop(&mut self) {
        if let Some(arguments) = self.applied.take() {
            release(vec![Retained::Application(arguments)]);
        }
    }
}

struct SyntaxComparer<'a> {
    first: &'a TypeGraph,
    second: &'a TypeGraph,
    declarations: HashMap<(TypeNodeId, TypeNodeId), bool>,
    // Root positions fit u32. Local ForAll symbols occupy a separate range and
    // are issued in pairs, independently of either substitution-frame depth.
    next_symbol: u64,
}
impl<'a> SyntaxComparer<'a> {
    fn new(first: &'a TypeGraph, second: &'a TypeGraph) -> Self {
        Self {
            first,
            second,
            declarations: HashMap::new(),
            next_symbol: 1_u64 << 32,
        }
    }

    fn declaration_equal(
        &mut self,
        first: TypeNodeId,
        second: TypeNodeId,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        budget.charge(1)?;
        if let Some(equal) = self.declarations.get(&(first, second)) {
            return Ok(*equal);
        }
        let equal = self
            .first
            .declaration_identity_eq(first, self.second, second, budget)?;
        self.declarations.insert((first, second), equal);
        Ok(equal)
    }

    fn arguments_equal(
        &mut self,
        first: &[Closure],
        second: &[Closure],
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        if first.len() != second.len() {
            return Ok(false);
        }
        budget.charge(first.len())?;
        self.equal_pairs(
            first.iter().cloned().zip(second.iter().cloned()).collect(),
            budget,
        )
    }

    fn equal(
        &mut self,
        first: Closure,
        second: Closure,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        self.equal_pairs(vec![(first, second)], budget)
    }

    fn equal_pairs(
        &mut self,
        mut pending: Vec<(Closure, Closure)>,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        let mut visited = HashSet::new();
        while let Some((first, second)) = pending.pop() {
            budget.charge(1)?;
            if !visited.insert((ScopedKey::new(&first), ScopedKey::new(&second))) {
                continue;
            }
            let first = resolve_head(self.first, first, budget)?;
            let second = resolve_head(self.second, second, budget)?;
            if first.arguments.len() != second.arguments.len() {
                return Ok(false);
            }
            budget.charge(first.arguments.len())?;
            pending.extend(
                first
                    .arguments
                    .iter()
                    .cloned()
                    .zip(second.arguments.iter().cloned()),
            );
            match (&first.kind, &second.kind) {
                (HeadKind::Nominal(first), HeadKind::Nominal(second)) => {
                    if !self.declaration_equal(*first, *second, budget)? {
                        return Ok(false);
                    }
                }
                (HeadKind::Unknown(first), HeadKind::Unknown(second)) if first == second => {}
                (HeadKind::Atomic(first), HeadKind::Atomic(second)) => {
                    let first_node = &self.first.graph()[first.expression];
                    let second_node = &self.second.graph()[second.expression];
                    budget.charge(
                        evidence_metadata_work(first_node).max(evidence_metadata_work(second_node)),
                    )?;
                    if first_node != second_node {
                        return Ok(false);
                    }
                    let forall = matches!(first_node, TypeNode::ForAll(_));
                    let symbol = self.next_symbol;
                    if forall {
                        self.next_symbol = self
                            .next_symbol
                            .checked_add(1)
                            .ok_or(TypeGraphError::TraversalWork)?;
                    }
                    let mut first_edges = self.first.ordered_edges(first.expression);
                    let mut second_edges = self.second.ordered_edges(second.expression);
                    loop {
                        budget.charge(1)?;
                        match (first_edges.next(), second_edges.next()) {
                            (None, None) => break,
                            (Some(first_edge), Some(second_edge))
                                if first_edge.weight() == second_edge.weight() =>
                            {
                                let mut first_env = first.environment.clone();
                                let mut second_env = second.environment.clone();
                                if forall && *first_edge.weight() == TypeEdge::Body {
                                    first_env = first_env.extend_unknown(symbol, budget)?;
                                    second_env = second_env.extend_unknown(symbol, budget)?;
                                }
                                budget.charge(1)?;
                                pending.push((
                                    Closure::new(first_edge.target(), first_env),
                                    Closure::new(second_edge.target(), second_env),
                                ));
                            }
                            _ => return Ok(false),
                        }
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn heads_equal(
        &mut self,
        first: &Head,
        second: &Head,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        if !self.arguments_equal(&first.arguments, &second.arguments, budget)? {
            return Ok(false);
        }
        match (&first.kind, &second.kind) {
            (HeadKind::Nominal(first), HeadKind::Nominal(second)) => {
                self.declaration_equal(*first, *second, budget)
            }
            (HeadKind::Unknown(first), HeadKind::Unknown(second)) => Ok(first == second),
            (HeadKind::Atomic(first), HeadKind::Atomic(second)) => {
                self.equal(first.clone(), second.clone(), budget)
            }
            _ => Ok(false),
        }
    }
}

fn compatible(
    first_graph: &TypeGraph,
    first: Closure,
    second_graph: &TypeGraph,
    second: Closure,
    budget: &mut TypeWorkBudget,
) -> Result<bool, TypeGraphError> {
    let mut syntax = SyntaxComparer::new(first_graph, second_graph);
    let mut pending = vec![(first, second)];
    let mut visited = HashSet::new();
    while let Some((first, second)) = pending.pop() {
        budget.charge(1)?;
        if !visited.insert((ScopedKey::new(&first), ScopedKey::new(&second))) {
            continue;
        }
        // Exact scoped agreement works even for an unconstructible recursive
        // newtype. Representation normalization is a separate fallback.
        if syntax.equal(first.clone(), second.clone(), budget)? {
            continue;
        }
        let Some(first) = weak_head(first_graph, first, budget)? else {
            return Ok(false);
        };
        let Some(second) = weak_head(second_graph, second, budget)? else {
            return Ok(false);
        };
        match (
            classify(first_graph, &first, budget)?,
            classify(second_graph, &second, budget)?,
        ) {
            (HeadClass::Data, HeadClass::Data) => {
                if !syntax.declaration_equal(
                    first.nominal().ok_or(TypeGraphError::InvalidRole(0))?,
                    second.nominal().ok_or(TypeGraphError::InvalidRole(0))?,
                    budget,
                )? {
                    return Ok(false);
                }
                if first.arguments.len() != second.arguments.len() {
                    return Ok(false);
                }
                budget.charge(first.arguments.len())?;
                pending.extend(
                    first
                        .arguments
                        .iter()
                        .cloned()
                        .zip(second.arguments.iter().cloned()),
                );
            }
            (HeadClass::Text, HeadClass::Text)
            | (HeadClass::Integer, HeadClass::Integer)
            | (HeadClass::Natural, HeadClass::Natural) => {}
            (HeadClass::Scalar(first), HeadClass::Scalar(second)) if first == second => {}
            _ => {
                if !syntax.heads_equal(&first, &second, budget)? {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{identity, physical};
    #[test]
    fn exact_wire_identity_checks_missing_and_nonroot_references() {
        let mut storage = GraphStorage::new();
        let text = declaration(&mut storage, "Text", 0, DeclarationForm::Text);
        let expression = application(&mut storage, text, &[]);
        let wire_root = root(&mut storage, expression, 0);
        let graph = publish(storage, &[]);
        let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
        assert!(graph
            .rooted_wire_identity_eq(wire_root, &graph, wire_root, &mut budget)
            .unwrap());
        for invalid in [
            WireTypeNodeId(u32::MAX),
            WireTypeNodeId(expression.index() as u32),
        ] {
            let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
            assert!(matches!(
                graph.rooted_wire_identity_eq(wire_root, &graph, invalid, &mut budget),
                Err(TypeGraphError::NotRoot(_))
            ));
        }
    }
    use super::super::{
        ForAllFlag, FunctionFlag, GraphLimits, GraphStorage, NominalHeadKind, ParameterFlag,
        RootDomain, SourceBinderFlag, TypeLiteral,
    };
    use super::*;
    use crate::execution_schema::ConstructorDecl;

    fn declaration(
        graph: &mut GraphStorage,
        name: &str,
        count: usize,
        form: DeclarationForm,
    ) -> TypeNodeId {
        let declaration = graph.add_node(TypeNode::Declaration {
            identity: identity(name, "type"),
            parameters: vec![ParameterFlag::NamedRequired; count],
            form,
            restriction: SyntaxRestriction::None,
        });
        if count > 0 {
            let kind = graph.add_node(TypeNode::Literal(TypeLiteral::Symbol("Type".into())));
            for index in 0..count {
                graph.add_edge(declaration, kind, TypeEdge::BinderKind(index as u32));
            }
        }
        declaration
    }
    fn application(
        graph: &mut GraphStorage,
        declaration: TypeNodeId,
        arguments: &[TypeNodeId],
    ) -> TypeNodeId {
        let expression = graph.add_node(TypeNode::NominalApplication);
        graph.add_edge(expression, declaration, TypeEdge::Head);
        for (index, argument) in arguments.iter().enumerate() {
            graph.add_edge(expression, *argument, TypeEdge::Argument(index as u32));
        }
        expression
    }
    fn root(graph: &mut GraphStorage, expression: TypeNodeId, binders: usize) -> WireTypeNodeId {
        let root = graph.add_node(TypeNode::Root {
            domain: if binders == 0 {
                RootDomain::Closed
            } else {
                RootDomain::ConstructorScheme
            },
            binders: vec![SourceBinderFlag::Specified; binders],
            rendered: "original reply".into(),
        });
        graph.add_edge(root, expression, TypeEdge::Body);
        if binders > 0 {
            let kind = graph.add_node(TypeNode::Literal(TypeLiteral::Symbol("Type".into())));
            for index in 0..binders {
                graph.add_edge(root, kind, TypeEdge::BinderKind(index as u32));
            }
        }
        WireTypeNodeId(root.index() as u32)
    }
    fn publish(graph: GraphStorage, inventory: &[ConstructorDecl]) -> Arc<TypeGraph> {
        Arc::new(TypeGraph::validate(graph, inventory, GraphLimits::default()).unwrap())
    }
    fn template(
        graph: &mut GraphStorage,
        declaration: TypeNodeId,
        constructor: ConstructorId,
        inventory: &[ConstructorDecl],
        fields: &[TypeNodeId],
    ) {
        let physical = &inventory[constructor.0 as usize];
        let template = graph.add_node(TypeNode::ConstructorTemplate {
            constructor,
            identity: physical.identity.clone(),
        });
        graph.add_edge(declaration, template, TypeEdge::Constructor(physical.tag));
        for (ordinal, field) in fields.iter().enumerate() {
            graph.add_edge(
                template,
                *field,
                TypeEdge::Field {
                    ordinal: ordinal as u32,
                    source_rep: physical.field_reps[ordinal],
                },
            );
        }
    }
    fn data(cursor: &TypeCursor, budget: &mut TypeWorkBudget) -> DataView {
        match cursor.view(budget).unwrap() {
            TypeView::Data(data) => data,
            other => panic!("expected data, got {other:?}"),
        }
    }
    fn function(graph: &mut GraphStorage, domain: TypeNodeId, codomain: TypeNodeId) -> TypeNodeId {
        let function = graph.add_node(TypeNode::Function(FunctionFlag::TypeToType));
        let multiplicity = graph.add_node(TypeNode::Literal(TypeLiteral::Natural("1".into())));
        graph.add_edge(function, multiplicity, TypeEdge::Multiplicity);
        graph.add_edge(function, domain, TypeEdge::Domain);
        graph.add_edge(function, codomain, TypeEdge::Codomain);
        function
    }

    #[test]
    fn alias_eta_prefix_reapplies_trailing_arguments_and_keeps_nominal_identity() {
        let mut graph = GraphStorage::new();
        let alias = declaration(
            &mut graph,
            "Apply",
            2,
            DeclarationForm::Newtype { eta_arity: 1 },
        );
        let prefix = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(alias, prefix, TypeEdge::AliasRhs);
        let target = declaration(
            &mut graph,
            "OpaqueTarget",
            1,
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "fixture".into(),
            },
        );
        let target_partial = application(&mut graph, target, &[]);
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let aliased = application(&mut graph, alias, &[target_partial, text_type]);
        let direct = application(&mut graph, target, &[text_type]);
        let aliased = root(&mut graph, aliased, 0);
        let direct = root(&mut graph, direct, 0);
        let graph = publish(graph, &[]);
        assert!(!graph
            .rooted_identity_eq(
                TypeNodeId::new(aliased.0 as usize),
                &graph,
                TypeNodeId::new(direct.0 as usize),
                &mut TypeWorkBudget::new(10_000)
            )
            .unwrap());
        assert!(graph
            .rooted_compatible(aliased, &graph, direct, &mut TypeWorkBudget::new(10_000))
            .unwrap());

        let mut graph = GraphStorage::new();
        let scalar = declaration(
            &mut graph,
            "IntPrim",
            0,
            DeclarationForm::Scalar(RuntimeRep::Int(64)),
        );
        let same_rep = declaration(
            &mut graph,
            "OtherIntPrim",
            0,
            DeclarationForm::Scalar(RuntimeRep::Int(64)),
        );
        let first = application(&mut graph, scalar, &[]);
        let second = application(&mut graph, same_rep, &[]);
        let first = root(&mut graph, first, 0);
        let second = root(&mut graph, second, 0);
        let graph = publish(graph, &[]);
        assert!(graph
            .rooted_compatible(first, &graph, second, &mut TypeWorkBudget::new(10_000))
            .unwrap());
        assert!(!graph
            .rooted_identity_eq(
                TypeNodeId::new(first.0 as usize),
                &graph,
                TypeNodeId::new(second.0 as usize),
                &mut TypeWorkBudget::new(10_000)
            )
            .unwrap());
    }

    #[test]
    fn selected_fieldless_branch_does_not_demand_unknown_or_function_payload() {
        let mut none = physical(vec![]);
        none.family_size = 2;
        let mut some = physical(vec![RuntimeRep::LiftedRef]);
        some.identity = identity("ReplySome", "data");
        some.host_id = crate::DataConId(78);
        some.tag = 2;
        some.family_size = 2;
        let inventory = vec![none, some];
        let mut graph = GraphStorage::new();
        let family = declaration(&mut graph, "Reply", 1, DeclarationForm::Data);
        let bound = graph.add_node(TypeNode::Bound(0));
        template(&mut graph, family, ConstructorId(0), &inventory, &[]);
        template(&mut graph, family, ConstructorId(1), &inventory, &[bound]);
        let unknown = application(&mut graph, family, &[bound]);
        let unknown = root(&mut graph, unknown, 1);
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let function = function(&mut graph, text_type, text_type);
        let known = application(&mut graph, family, &[function]);
        let known = root(&mut graph, known, 0);
        let graph = publish(graph, &inventory);
        for root in [unknown, known] {
            let mut budget = TypeWorkBudget::new(10_000);
            let cursor = graph.open_root(root, &mut budget).unwrap();
            let data = data(&cursor, &mut budget);
            assert_eq!(
                data.constructors().collect::<Vec<_>>(),
                vec![ConstructorId(0), ConstructorId(1)]
            );
            assert_eq!(data.argument_count(), 1);
            assert!(data
                .fields(ConstructorId(0), &mut budget)
                .unwrap()
                .unwrap()
                .is_empty());
            let fields = data.fields(ConstructorId(1), &mut budget).unwrap().unwrap();
            assert!(matches!(
                fields[0].view(&mut budget).unwrap(),
                TypeView::Unconstructible(_)
            ));
            assert!(data
                .fields(ConstructorId(99), &mut budget)
                .unwrap()
                .is_none());
        }
    }

    fn recursive_fixture(
        extra: bool,
    ) -> (Arc<TypeGraph>, WireTypeNodeId, ConstructorId, ConstructorId) {
        let mut nest = physical(vec![RuntimeRep::LiftedRef; 2]);
        nest.identity = identity("Nest", "data");
        let mut pair = physical(vec![RuntimeRep::LiftedRef; 2]);
        pair.identity = identity("Pair", "data");
        pair.family = identity("Pair", "type");
        pair.host_id = crate::DataConId(78);
        let mut inventory = vec![nest, pair];
        let (nest_id, pair_id) = if extra {
            let mut unused = physical(vec![]);
            unused.identity = identity("Unused", "data");
            unused.family = identity("Unused", "type");
            unused.host_id = crate::DataConId(79);
            inventory.insert(0, unused);
            (ConstructorId(1), ConstructorId(2))
        } else {
            (ConstructorId(0), ConstructorId(1))
        };
        let mut graph = GraphStorage::new();
        if extra {
            graph.add_node(TypeNode::Literal(TypeLiteral::Natural("9".into())));
        }
        let nest = declaration(&mut graph, "Reply", 1, DeclarationForm::Data);
        let pair = declaration(&mut graph, "Pair", 2, DeclarationForm::Data);
        let newest = graph.add_node(TypeNode::Bound(0));
        let older = graph.add_node(TypeNode::Bound(1));
        let duplicated = application(&mut graph, pair, &[newest, newest]);
        let next = application(&mut graph, nest, &[duplicated]);
        template(&mut graph, nest, nest_id, &inventory, &[newest, next]);
        template(&mut graph, pair, pair_id, &inventory, &[older, newest]);
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let body = application(&mut graph, nest, &[text_type]);
        let root = root(&mut graph, body, 0);
        (publish(graph, &inventory), root, nest_id, pair_id)
    }

    #[test]
    fn parameter_changing_recursion_is_demanded_finitely_with_shared_arguments() {
        let (graph, root, nest_id, pair_id) = recursive_fixture(false);
        let (other, other_root, _, _) = recursive_fixture(true);
        let size = graph.graph().node_count();
        assert!(graph
            .rooted_compatible(root, &other, other_root, &mut TypeWorkBudget::new(100_000))
            .unwrap());
        let mut budget = TypeWorkBudget::new(100_000);
        let mut cursor = graph.open_root(root, &mut budget).unwrap();
        for _ in 0..12 {
            let nest = data(&cursor, &mut budget);
            let fields = nest.fields(nest_id, &mut budget).unwrap().unwrap();
            cursor = fields[1].clone();
        }
        let nest = data(&cursor, &mut budget);
        let fields = nest.fields(nest_id, &mut budget).unwrap().unwrap();
        let pair = data(&fields[0], &mut budget);
        assert!(Arc::ptr_eq(
            pair.arguments[0].environment.0.as_ref().unwrap(),
            pair.arguments[1].environment.0.as_ref().unwrap()
        ));
        let fields = pair.fields(pair_id, &mut budget).unwrap().unwrap();
        assert!(Arc::ptr_eq(
            fields[0].closure.environment.0.as_ref().unwrap(),
            fields[1].closure.environment.0.as_ref().unwrap()
        ));
        assert_eq!(graph.graph().node_count(), size);
    }

    #[test]
    fn effect_summary_uses_result_syntax_and_forall_kind_but_not_metadata_or_fields() {
        let inventory = vec![physical(vec![RuntimeRep::LiftedRef])];
        let mut graph = GraphStorage::new();
        let effect = declaration(
            &mut graph,
            "Eff",
            0,
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "effect".into(),
            },
        );
        if let TypeNode::Declaration { restriction, .. } = graph.node_weight_mut(effect).unwrap() {
            *restriction = SyntaxRestriction::EffectHead;
        }
        let eff = application(&mut graph, effect, &[]);
        let family = declaration(&mut graph, "Reply", 0, DeclarationForm::Data);
        template(&mut graph, family, ConstructorId(0), &inventory, &[eff]);
        let body = application(&mut graph, family, &[]);
        let fields_only = root(&mut graph, body, 0);
        let forall = graph.add_node(TypeNode::ForAll(ForAllFlag::Specified));
        let bound = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(forall, eff, TypeEdge::Kind);
        graph.add_edge(forall, bound, TypeEdge::Body);
        let explicit_kind = root(&mut graph, forall, 0);
        let metadata = graph.add_node(TypeNode::Root {
            domain: RootDomain::ConstructorScheme,
            binders: vec![SourceBinderFlag::Specified],
            rendered: "a".into(),
        });
        graph.add_edge(metadata, eff, TypeEdge::BinderKind(0));
        graph.add_edge(metadata, bound, TypeEdge::Body);
        let graph = publish(graph, &inventory);
        let mut budget = TypeWorkBudget::new(20_000);
        let data = data(
            &graph.open_root(fields_only, &mut budget).unwrap(),
            &mut budget,
        );
        let field = data.fields(ConstructorId(0), &mut budget).unwrap().unwrap();
        assert!(matches!(
            field[0].view(&mut budget).unwrap(),
            TypeView::Unconstructible(ConstructionRefusal::Effectful)
        ));
        assert!(matches!(
            graph
                .open_root(explicit_kind, &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Unconstructible(ConstructionRefusal::Effectful)
        ));
        assert!(matches!(
            graph
                .open_root(WireTypeNodeId(metadata.index() as u32), &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Unconstructible(ConstructionRefusal::Polymorphic)
        ));
    }

    #[test]
    fn head_alias_normalizes_before_effect_check_while_nested_alias_rhs_stays_unvisited() {
        let mut graph = GraphStorage::new();
        let effect = declaration(
            &mut graph,
            "Eff",
            0,
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "effect".into(),
            },
        );
        if let TypeNode::Declaration { restriction, .. } = graph.node_weight_mut(effect).unwrap() {
            *restriction = SyntaxRestriction::EffectHead;
        }
        let eff = application(&mut graph, effect, &[]);
        let alias = declaration(
            &mut graph,
            "HiddenEff",
            0,
            DeclarationForm::Newtype { eta_arity: 0 },
        );
        graph.add_edge(alias, eff, TypeEdge::AliasRhs);
        let alias_type = application(&mut graph, alias, &[]);
        let direct = root(&mut graph, alias_type, 0);
        let opaque = declaration(
            &mut graph,
            "Opaque",
            1,
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Family,
                reason: "type family".into(),
            },
        );
        let nested = application(&mut graph, opaque, &[alias_type]);
        let nested = root(&mut graph, nested, 0);
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let marked_alias = declaration(
            &mut graph,
            "MarkedAlias",
            0,
            DeclarationForm::Newtype { eta_arity: 0 },
        );
        if let TypeNode::Declaration { restriction, .. } =
            graph.node_weight_mut(marked_alias).unwrap()
        {
            *restriction = SyntaxRestriction::EffectHead;
        }
        graph.add_edge(marked_alias, text_type, TypeEdge::AliasRhs);
        let marked = application(&mut graph, marked_alias, &[]);
        let marked = root(&mut graph, marked, 0);
        let graph = publish(graph, &[]);
        let mut budget = TypeWorkBudget::new(20_000);
        assert!(matches!(
            graph
                .open_root(direct, &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Unconstructible(ConstructionRefusal::Effectful)
        ));
        assert!(matches!(
            graph
                .open_root(nested, &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Unconstructible(ConstructionRefusal::Declared { .. })
        ));
        assert!(matches!(
            graph
                .open_root(marked, &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Text
        ));
    }

    #[test]
    fn opaque_function_arguments_do_not_gain_nested_newtype_transparency() {
        let mut graph = GraphStorage::new();
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let alias = declaration(
            &mut graph,
            "WrappedText",
            0,
            DeclarationForm::Newtype { eta_arity: 0 },
        );
        graph.add_edge(alias, text_type, TypeEdge::AliasRhs);
        let alias_type = application(&mut graph, alias, &[]);
        let first = function(&mut graph, alias_type, text_type);
        let second = function(&mut graph, text_type, text_type);
        let first = root(&mut graph, first, 0);
        let second = root(&mut graph, second, 0);
        let family = declaration(
            &mut graph,
            "F",
            1,
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Family,
                reason: "same refusal".into(),
            },
        );
        let third = application(&mut graph, family, &[alias_type]);
        let fourth = application(&mut graph, family, &[text_type]);
        let third = root(&mut graph, third, 0);
        let fourth = root(&mut graph, fourth, 0);
        let graph = publish(graph, &[]);
        assert!(!graph
            .rooted_compatible(first, &graph, second, &mut TypeWorkBudget::new(20_000))
            .unwrap());
        assert!(!graph
            .rooted_compatible(third, &graph, fourth, &mut TypeWorkBudget::new(20_000))
            .unwrap());
    }

    #[test]
    fn identical_recursive_alias_is_compatible_but_demand_refuses_and_work_errors_propagate() {
        let mut graph = GraphStorage::new();
        let alias = declaration(
            &mut graph,
            "Loop",
            0,
            DeclarationForm::Newtype { eta_arity: 0 },
        );
        let body = application(&mut graph, alias, &[]);
        graph.add_edge(alias, body, TypeEdge::AliasRhs);
        let root = root(&mut graph, body, 0);
        let graph = publish(graph, &[]);
        assert!(graph
            .rooted_compatible(root, &graph, root, &mut TypeWorkBudget::new(10_000))
            .unwrap());
        let mut budget = TypeWorkBudget::new(10_000);
        let cursor = graph.open_root(root, &mut budget).unwrap();
        assert!(matches!(
            cursor.view(&mut budget).unwrap(),
            TypeView::Unconstructible(ConstructionRefusal::RecursiveNewtype)
        ));
        assert!(matches!(
            cursor.view(&mut TypeWorkBudget::new(0)),
            Err(TypeGraphError::TraversalWork)
        ));
        assert!(matches!(
            graph.rooted_compatible(root, &graph, root, &mut TypeWorkBudget::new(0)),
            Err(TypeGraphError::TraversalWork)
        ));
    }

    #[test]
    fn repeated_finite_identity_aliases_are_not_a_declaration_cycle() {
        let mut graph = GraphStorage::new();
        let id = declaration(
            &mut graph,
            "Id",
            1,
            DeclarationForm::Newtype { eta_arity: 1 },
        );
        let bound = graph.add_node(TypeNode::Bound(0));
        graph.add_edge(id, bound, TypeEdge::AliasRhs);
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let inner = application(&mut graph, id, &[text_type]);
        let outer = application(&mut graph, id, &[inner]);
        let outer = root(&mut graph, outer, 0);
        let direct = root(&mut graph, text_type, 0);
        let graph = publish(graph, &[]);
        let mut budget = TypeWorkBudget::new(20_000);
        assert!(matches!(
            graph
                .open_root(outer, &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Text
        ));
        assert!(graph
            .rooted_compatible(outer, &graph, direct, &mut budget)
            .unwrap());
    }

    #[test]
    fn distinct_alias_chain_does_not_compare_unrelated_instantiation_history() {
        let mut graph = GraphStorage::new();
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let mut body = application(&mut graph, text, &[]);
        for index in 0..1_000 {
            let alias = declaration(
                &mut graph,
                &format!("Alias{index}"),
                0,
                DeclarationForm::Newtype { eta_arity: 0 },
            );
            graph.add_edge(alias, body, TypeEdge::AliasRhs);
            body = application(&mut graph, alias, &[]);
        }
        let root = root(&mut graph, body, 0);
        let graph = publish(graph, &[]);
        let mut budget = TypeWorkBudget::new(20_000);
        assert!(matches!(
            graph
                .open_root(root, &mut budget)
                .unwrap()
                .view(&mut budget)
                .unwrap(),
            TypeView::Text
        ));
    }

    #[test]
    fn growing_alias_stops_with_a_work_error_instead_of_an_equal_refusal_shape() {
        let mut graph = GraphStorage::new();
        let alias = declaration(
            &mut graph,
            "Grow",
            1,
            DeclarationForm::Newtype { eta_arity: 1 },
        );
        let pair = declaration(
            &mut graph,
            "Pair",
            2,
            DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "fixture".into(),
            },
        );
        let bound = graph.add_node(TypeNode::Bound(0));
        let pair_type = application(&mut graph, pair, &[bound, bound]);
        let next = application(&mut graph, alias, &[pair_type]);
        graph.add_edge(alias, next, TypeEdge::AliasRhs);
        let text = declaration(&mut graph, "Text", 0, DeclarationForm::Text);
        let text_type = application(&mut graph, text, &[]);
        let body = application(&mut graph, alias, &[text_type]);
        let root = root(&mut graph, body, 0);
        let graph = publish(graph, &[]);
        let mut budget = TypeWorkBudget::new(4_000);
        let cursor = graph.open_root(root, &mut budget).unwrap();
        assert!(matches!(
            cursor.view(&mut budget),
            Err(TypeGraphError::TraversalWork)
        ));
        assert!(graph
            .rooted_compatible(root, &graph, root, &mut TypeWorkBudget::new(10_000))
            .unwrap());
    }

    #[test]
    fn forall_local_symbols_do_not_alias_original_root_binders() {
        let mut graph = GraphStorage::new();
        let kind = graph.add_node(TypeNode::Literal(TypeLiteral::Symbol("Type".into())));
        let local = graph.add_node(TypeNode::Bound(0));
        let outer = graph.add_node(TypeNode::Bound(1));
        let first = graph.add_node(TypeNode::ForAll(ForAllFlag::Specified));
        graph.add_edge(first, kind, TypeEdge::Kind);
        graph.add_edge(first, local, TypeEdge::Body);
        let second = graph.add_node(TypeNode::ForAll(ForAllFlag::Specified));
        graph.add_edge(second, kind, TypeEdge::Kind);
        graph.add_edge(second, outer, TypeEdge::Body);
        let first = root(&mut graph, first, 1);
        let second = root(&mut graph, second, 1);
        let graph = publish(graph, &[]);
        assert!(!graph
            .rooted_compatible(first, &graph, second, &mut TypeWorkBudget::new(20_000))
            .unwrap());
        assert!(graph
            .rooted_compatible(first, &graph, first, &mut TypeWorkBudget::new(20_000))
            .unwrap());
    }

    #[test]
    fn retained_environment_and_application_guards_drop_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut budget = TypeWorkBudget::new(100_000);
                let mut environment = Environment::default();
                for _ in 0..20_000 {
                    let argument = Closure::new(TypeNodeId::new(0), environment.clone());
                    environment = Environment::arguments(&[argument], &mut budget).unwrap();
                }
                let mut closure = Closure::new(TypeNodeId::new(0), environment);
                for _ in 0..20_000 {
                    closure = Closure {
                        expression: TypeNodeId::new(0),
                        environment: Environment::default(),
                        applied: Some(Arc::new(ApplicationArguments(
                            vec![closure].into_boxed_slice(),
                        ))),
                    };
                }
                let guard = ScopedKey::new(&closure);
                let pending: Arc<[Closure]> = vec![closure].into();
                drop(pending);
                drop(guard);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
