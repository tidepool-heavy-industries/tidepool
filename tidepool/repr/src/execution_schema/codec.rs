use crate::type_graph::{
    DeclarationForm, ForAllFlag, FunctionFlag, GraphLimits, GraphStorage, NominalHeadKind,
    ParameterFlag, RootDomain, SourceBinderFlag, SyntaxRestriction, TypeEdge, TypeGraph,
    TypeGraphError, TypeLiteral,
};
use petgraph::visit::EdgeRef;
use std::io::Cursor;
use std::sync::Arc;

use ciborium::value::Value;

use super::{
    Alternative, AlternativePattern, Architecture, Atom, CaseKind, CheckedLayout, ConstructorDecl,
    ConstructorId, ConstructorReply, DecodeLimits, Endianness, Expr, ExprFrame, FieldLayout,
    GlobalDecl, GlobalId, Group, HeapBinding, HeapRhs, JoinBinding, JoinId, OperationBudget,
    OperationDecl, OperationId, ParseError, ProgramDefinitions, ProgramEnvelope, RuntimeRep,
    ScalarLiteral, Signature, SignatureId, SiteDelivery, SiteRow, SymbolIdentity, TargetDescriptor,
    TopBinding, TypeNode, TypeNodeId, UpdatePolicy, ValueId, ValueRef, WireProgram,
};

pub(super) fn type_graph_error(error: TypeGraphError) -> ParseError {
    match error {
        TypeGraphError::Limit(limit) => ParseError::LimitExceeded(limit),
        TypeGraphError::TraversalWork => ParseError::LimitExceeded("work"),
        error @ (TypeGraphError::InvalidReference(_) | TypeGraphError::NotRoot(_)) => {
            ParseError::InvalidReference(error.to_string())
        }
        error @ TypeGraphError::InvalidScope(_) => ParseError::InvalidScope(error.to_string()),
        error @ TypeGraphError::DuplicateDeclaration(_) => {
            ParseError::DuplicateDefinition(error.to_string())
        }
        error @ TypeGraphError::InvalidLiteral(_) => ParseError::Malformed(error.to_string()),
        error @ (TypeGraphError::InvalidRole(_)
        | TypeGraphError::InvalidCardinality(_)
        | TypeGraphError::InvalidIdentity(_)
        | TypeGraphError::InvalidRepresentation(_)
        | TypeGraphError::InvalidConstructor(_)
        | TypeGraphError::ExpressionCycle) => ParseError::InvalidLayout(error.to_string()),
    }
}

pub(super) fn encode_type_graph_value(graph: &TypeGraph) -> Value {
    fn n(value: impl Into<u64>) -> Value {
        Value::Integer(value.into().into())
    }
    fn text(value: &str) -> Value {
        Value::Text(value.into())
    }
    fn a(values: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(values.into_iter().collect())
    }
    fn rep(value: RuntimeRep) -> Value {
        match value {
            RuntimeRep::Void => a([n(0_u64)]),
            RuntimeRep::LiftedRef => a([n(1_u64)]),
            RuntimeRep::UnliftedRef => a([n(2_u64)]),
            RuntimeRep::Address => a([n(3_u64)]),
            RuntimeRep::Int(bits) => a([n(4_u64), n(bits)]),
            RuntimeRep::Word(bits) => a([n(5_u64), n(bits)]),
            RuntimeRep::Float(bits) => a([n(6_u64), n(bits)]),
        }
    }
    fn symbol(value: &SymbolIdentity) -> Value {
        let SymbolIdentity {
            unit,
            module,
            namespace,
            occurrence,
            record_parent,
        } = value;
        a([
            text(unit),
            text(module),
            text(namespace),
            text(occurrence),
            match record_parent {
                None => a([n(0_u64)]),
                Some(parent) => a([n(1_u64), text(parent)]),
            },
        ])
    }
    fn form(value: &DeclarationForm) -> Value {
        match value {
            DeclarationForm::Data => a([n(0_u64)]),
            DeclarationForm::Newtype { eta_arity } => a([n(1_u64), n(*eta_arity)]),
            DeclarationForm::Text => a([n(2_u64)]),
            DeclarationForm::Integer => a([n(3_u64)]),
            DeclarationForm::Natural => a([n(4_u64)]),
            DeclarationForm::Scalar(value) => a([n(5_u64), rep(*value)]),
            DeclarationForm::Opaque { head_kind, reason } => a([
                n(6_u64),
                n(match head_kind {
                    NominalHeadKind::Constructor => 0_u64,
                    NominalHeadKind::Family => 1,
                }),
                text(reason),
            ]),
        }
    }
    fn node(value: &TypeNode) -> Value {
        match value {
            TypeNode::Root {
                domain,
                binders,
                rendered,
            } => a([
                n(0_u64),
                n(match domain {
                    RootDomain::Closed => 0_u64,
                    RootDomain::ConstructorScheme => 1,
                }),
                a(binders.iter().map(|flag| {
                    n(match flag {
                        SourceBinderFlag::Specified => 1_u64,
                        SourceBinderFlag::Inferred => 2,
                    })
                })),
                text(rendered),
            ]),
            TypeNode::Declaration {
                identity,
                parameters,
                form: declaration,
                restriction,
            } => a([
                n(1_u64),
                symbol(identity),
                a(parameters.iter().map(|flag| {
                    n(match flag {
                        ParameterFlag::NamedRequired => 0_u64,
                        ParameterFlag::NamedSpecified => 1,
                        ParameterFlag::NamedInferred => 2,
                        ParameterFlag::AnonymousVisible => 3,
                    })
                })),
                form(declaration),
                n(match restriction {
                    SyntaxRestriction::None => 0_u64,
                    SyntaxRestriction::EffectHead => 1,
                }),
            ]),
            TypeNode::ConstructorTemplate {
                constructor,
                identity: _,
            } => a([n(2_u64), n(constructor.0)]),
            TypeNode::Bound(index) => a([n(3_u64), n(*index)]),
            TypeNode::NominalApplication => a([n(4_u64)]),
            TypeNode::Application => a([n(5_u64)]),
            TypeNode::Function(flag) => a([
                n(6_u64),
                n(match flag {
                    FunctionFlag::TypeToType => 0_u64,
                    FunctionFlag::TypeToConstraint => 1,
                    FunctionFlag::ConstraintToType => 2,
                    FunctionFlag::ConstraintToConstraint => 3,
                }),
            ]),
            TypeNode::ForAll(flag) => a([
                n(7_u64),
                n(match flag {
                    ForAllFlag::Required => 0_u64,
                    ForAllFlag::Specified => 1,
                    ForAllFlag::Inferred => 2,
                }),
            ]),
            TypeNode::Literal(literal) => match literal {
                TypeLiteral::Natural(value) => a([n(8_u64), n(0_u64), text(value)]),
                TypeLiteral::Symbol(value) => a([n(8_u64), n(1_u64), text(value)]),
                TypeLiteral::Character(value) => a([n(8_u64), n(2_u64), n(*value as u32)]),
            },
        }
    }
    fn role(value: TypeEdge) -> Value {
        match value {
            TypeEdge::BinderKind(ordinal) => a([n(0_u64), n(ordinal)]),
            TypeEdge::Body => a([n(1_u64)]),
            TypeEdge::Head => a([n(2_u64)]),
            TypeEdge::Argument(ordinal) => a([n(3_u64), n(ordinal)]),
            TypeEdge::Function => a([n(4_u64)]),
            TypeEdge::ApplyArgument => a([n(5_u64)]),
            TypeEdge::Multiplicity => a([n(6_u64)]),
            TypeEdge::Domain => a([n(7_u64)]),
            TypeEdge::Codomain => a([n(8_u64)]),
            TypeEdge::Kind => a([n(9_u64)]),
            TypeEdge::Constructor(tag) => a([n(10_u64), n(tag)]),
            TypeEdge::Field {
                ordinal,
                source_rep,
            } => a([n(11_u64), n(ordinal), rep(source_rep)]),
            TypeEdge::AliasRhs => a([n(12_u64)]),
        }
    }
    a([
        n(1_u64),
        a(graph.graph().node_weights().map(node)),
        a(graph.graph().node_indices().flat_map(|source| {
            graph.ordered_edges(source).map(move |edge| {
                a([
                    n(source.index() as u64),
                    n(edge.target().index() as u64),
                    role(*edge.weight()),
                ])
            })
        })),
    ])
}

// Flat schema records have bounded container nesting regardless of program
// depth. This is a malformed-wire guard, not an expression complexity limit.
const MAX_CONTAINER_NESTING: usize = 32;

/// Decode only the closed flat CBOR grammar into an unpublished wire value.
/// Semantic validation and construction publication remain in `decode`.
pub(super) fn decode_wire(
    bytes: &[u8],
    limits: DecodeLimits,
    budget: &mut OperationBudget,
) -> Result<WireProgram, ParseError> {
    let value = decode_value(bytes, limits, budget)?;
    Decoder::new(limits, budget).program(&value)
}

/// Decode an entry-free original STG group using the same bounded table and
/// expression grammar as a complete program.
pub(super) fn decode_group_wire(
    bytes: &[u8],
    limits: DecodeLimits,
    budget: &mut OperationBudget,
) -> Result<(u32, Vec<SymbolIdentity>, ProgramDefinitions), ParseError> {
    let Value::Array(fields) = decode_value(bytes, limits, budget)? else {
        return Err(malformed("projected group", "array"));
    };
    if fields.len() != 19 || text_raw(&fields[0], "group magic")? != "TPGRP" {
        return Err(ParseError::Malformed(
            "invalid projected group header".into(),
        ));
    }
    if unsigned(&fields[1], "group schema version")? != 1 {
        return Err(ParseError::Malformed(
            "unsupported projected group version".into(),
        ));
    }
    let ordinal = u32_value(&fields[2], "original group ordinal")?;
    let mut decoder = Decoder::new(limits, budget);
    let binders = decoder.list(&fields[3], true, |this, value| this.symbol(value))?;
    if binders.is_empty() {
        return Err(ParseError::Malformed(
            "projected group has no binders".into(),
        ));
    }
    let definitions = decoder.definitions(&fields[4..].iter().collect::<Vec<_>>())?;
    let top_symbols: std::collections::BTreeSet<_> = definitions
        .bindings
        .iter()
        .flat_map(|group| match group {
            Group::NonRecursive(top) => std::slice::from_ref(top),
            Group::Recursive(tops) => tops.as_slice(),
        })
        .map(|top| &top.identity)
        .collect();
    for binder in &binders {
        if !top_symbols.contains(binder) {
            return Err(ParseError::InvalidReference(format!(
                "group binder {binder:?} is absent"
            )));
        }
    }
    Ok((ordinal, binders, definitions))
}

pub(super) fn decode_value(
    bytes: &[u8],
    limits: DecodeLimits,
    budget: &mut OperationBudget,
) -> Result<Value, ParseError> {
    if bytes.len() > limits.max_bytes {
        return Err(ParseError::ByteLimit {
            limit: limits.max_bytes,
            actual: bytes.len(),
        });
    }
    let consumed = scan_item(bytes, budget)?;
    if consumed != bytes.len() {
        return Err(ParseError::TrailingBytes);
    }

    let mut cursor = Cursor::new(bytes);
    let value: Value =
        ciborium::de::from_reader_with_recursion_limit(&mut cursor, MAX_CONTAINER_NESTING)
            .map_err(|error| match error {
                ciborium::de::Error::Io(_) | ciborium::de::Error::Syntax(_) => {
                    ParseError::Truncated
                }
                ciborium::de::Error::RecursionLimitExceeded => ParseError::LimitExceeded("depth"),
                ciborium::de::Error::Semantic(_, detail) => ParseError::Malformed(detail),
            })?;
    if cursor.position() as usize != bytes.len() {
        return Err(ParseError::TrailingBytes);
    }
    Ok(value)
}

/// Walk one complete CBOR item, rejecting indefinite containers before
/// `ciborium::Value` erases that distinction.
fn scan_item(bytes: &[u8], budget: &mut OperationBudget) -> Result<usize, ParseError> {
    let mut remaining = vec![1_u64];
    let mut offset = 0_usize;
    while let Some(items) = remaining.last_mut() {
        if *items == 0 {
            remaining.pop();
            continue;
        }
        *items -= 1;
        budget.charge(1)?;
        let initial = *bytes.get(offset).ok_or(ParseError::Truncated)?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        if additional == 31 {
            return Err(ParseError::Malformed(
                "indefinite-length CBOR is not supported".into(),
            ));
        }
        let (argument, head) = cbor_argument(bytes, offset, additional)?;
        offset = offset
            .checked_add(head)
            .ok_or(ParseError::LimitExceeded("work"))?;
        let children = match major {
            0 | 1 | 7 => 0,
            2 | 3 => {
                // Reserve raw Value payload copies before ciborium allocates.
                budget.charge(
                    usize::try_from(argument).map_err(|_| ParseError::LimitExceeded("work"))?,
                )?;
                offset = offset
                    .checked_add(
                        usize::try_from(argument).map_err(|_| ParseError::LimitExceeded("work"))?,
                    )
                    .filter(|end| *end <= bytes.len())
                    .ok_or(ParseError::Truncated)?;
                0
            }
            4 => {
                budget.reserve::<Value>(
                    usize::try_from(argument).map_err(|_| ParseError::LimitExceeded("work"))?,
                )?;
                argument
            }
            5 => {
                budget.reserve::<(Value, Value)>(
                    usize::try_from(argument).map_err(|_| ParseError::LimitExceeded("work"))?,
                )?;
                argument
                    .checked_mul(2)
                    .ok_or(ParseError::LimitExceeded("work"))?
            }
            6 => 1,
            _ => return Err(ParseError::Malformed("invalid CBOR major type".into())),
        };
        if children != 0 {
            if remaining.len() >= MAX_CONTAINER_NESTING {
                return Err(ParseError::Malformed(
                    "flat schema container nesting exceeded".into(),
                ));
            }
            remaining.push(children);
        }
    }
    Ok(offset)
}

fn cbor_argument(bytes: &[u8], offset: usize, additional: u8) -> Result<(u64, usize), ParseError> {
    let read = |count: usize| {
        let start = offset + 1;
        let end = start.checked_add(count).ok_or(ParseError::Truncated)?;
        bytes.get(start..end).ok_or(ParseError::Truncated)
    };
    match additional {
        value @ 0..=23 => Ok((u64::from(value), 1)),
        24 => Ok((u64::from(read(1)?[0]), 2)),
        25 => Ok((
            u64::from(u16::from_be_bytes(
                read(2)?.try_into().map_err(|_| ParseError::Truncated)?,
            )),
            3,
        )),
        26 => Ok((
            u64::from(u32::from_be_bytes(
                read(4)?.try_into().map_err(|_| ParseError::Truncated)?,
            )),
            5,
        )),
        27 => Ok((
            u64::from_be_bytes(read(8)?.try_into().map_err(|_| ParseError::Truncated)?),
            9,
        )),
        _ => Err(ParseError::Malformed(
            "reserved CBOR additional information".into(),
        )),
    }
}

struct Decoder<'a> {
    limits: DecodeLimits,
    nodes: usize,
    tables: usize,
    strings: usize,
    budget: &'a mut OperationBudget,
}

impl<'a> Decoder<'a> {
    fn new(limits: DecodeLimits, budget: &'a mut OperationBudget) -> Self {
        Self {
            limits,
            nodes: 0,
            tables: 0,
            strings: 0,
            budget,
        }
    }

    fn charge(&mut self, amount: usize) -> Result<(), ParseError> {
        self.budget.charge(amount)
    }

    fn node(&mut self) -> Result<(), ParseError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(ParseError::LimitExceeded("nodes"))?;
        if self.nodes > self.limits.max_nodes {
            return Err(ParseError::LimitExceeded("nodes"));
        }
        self.charge(1)
    }

    fn table(&mut self, len: usize) -> Result<(), ParseError> {
        self.tables = self
            .tables
            .checked_add(len)
            .ok_or(ParseError::LimitExceeded("table entries"))?;
        if self.tables > self.limits.max_table_entries {
            return Err(ParseError::LimitExceeded("table entries"));
        }
        self.charge(len)
    }

    fn program(&mut self, value: &Value) -> Result<WireProgram, ParseError> {
        let Value::Array(header) = value else {
            return Err(malformed("program", "array"));
        };
        if header.len() < 2 {
            return Err(ParseError::Malformed("wrong program field count".into()));
        }
        if text_raw(&header[0], "program magic")? != "TPSTG" {
            return Err(ParseError::Malformed(
                "invalid prepared program magic".into(),
            ));
        }
        let schema_version = unsigned(&header[1], "schema version")?;
        if schema_version != super::SCHEMA_VERSION {
            return Err(ParseError::UnsupportedVersion(schema_version));
        }
        let fields = array(value, 17, "program")?;
        let definition_fields = fields[1..12]
            .iter()
            .chain(fields[13..].iter())
            .collect::<Vec<_>>();
        let definitions = self.definitions(&definition_fields)?;
        Ok(definitions.into_wire(ValueId(u32_value(&fields[12], "entry value ID")?)))
    }

    fn definitions(&mut self, fields: &[&Value]) -> Result<ProgramDefinitions, ParseError> {
        if fields.len() != 15 {
            return Err(ParseError::Malformed(
                "wrong definitions field count".into(),
            ));
        }
        let schema_version = unsigned(fields[0], "schema version")?;
        if schema_version != super::SCHEMA_VERSION {
            return Err(ParseError::UnsupportedVersion(schema_version));
        }
        let target = self.target(fields[4])?;
        let signatures = self.list(fields[5], true, |this, value| this.signature(value))?;
        let globals = self.list(fields[6], true, |this, value| this.global(value))?;
        let constructors = self.list(fields[7], true, |this, value| this.constructor(value))?;
        let operations = self.list(fields[8], true, |this, value| this.operation(value))?;
        let expressions = self.expr(fields[9])?;
        let bindings = self.list(fields[10], true, |this, value| this.top_group(value))?;
        let types = self.type_graph(fields[11], &constructors)?;
        let sites = self.sites(fields[12])?;
        let constructor_replies = self.constructor_replies(fields[13])?;
        let json_layout = self.optional_json_layout(fields[14])?;
        Ok(ProgramDefinitions {
            envelope: ProgramEnvelope {
                schema_version,
                projection_profile: self.text(fields[1], "projection profile")?,
                toolchain: self.text(fields[2], "toolchain")?,
                execution_abi_version: unsigned(fields[3], "execution ABI version")?,
                target,
            },
            signatures,
            globals,
            constructors,
            operations,
            expressions,
            bindings,
            types,
            sites,
            constructor_replies,
            json_layout,
        })
    }

    fn type_graph(
        &mut self,
        value: &Value,
        constructors: &[ConstructorDecl],
    ) -> Result<Arc<TypeGraph>, ParseError> {
        let fields = array(value, 3, "finite type graph")?;
        if unsigned(&fields[0], "finite type graph version")? != 1 {
            return Err(ParseError::Malformed(
                "unsupported finite type graph version".into(),
            ));
        }
        let nodes = fields[1]
            .as_array()
            .ok_or_else(|| malformed("finite type nodes", "array"))?;
        let edges = fields[2]
            .as_array()
            .ok_or_else(|| malformed("finite type edges", "array"))?;
        if nodes.len() > self.limits.max_type_nodes {
            return Err(ParseError::LimitExceeded("type nodes"));
        }
        self.table(nodes.len())?;
        self.table(edges.len())?;
        let mut graph = GraphStorage::with_capacity(nodes.len(), edges.len());
        let mut templates = std::collections::BTreeSet::new();
        for node in nodes {
            let fields = tagged(node, "finite type node")?;
            if fields.len() == 2 && unsigned(&fields[0], "finite type node tag")? == 2 {
                let constructor = ConstructorId(u32_value(&fields[1], "template constructor ID")?);
                // Derive each original identity at most once before allocating
                // its clone. Repeated small references cannot multiply a large
                // physical symbol's strings before graph publication refuses it.
                if !templates.insert(constructor) {
                    return Err(ParseError::DuplicateDefinition(
                        "type constructor template".into(),
                    ));
                }
            }
            graph.add_node(self.type_node(node, constructors)?);
        }
        for edge in edges {
            let fields = array(edge, 3, "finite type edge")?;
            let source = u32_value(&fields[0], "type edge source")? as usize;
            let target = u32_value(&fields[1], "type edge target")? as usize;
            if source >= nodes.len() || target >= nodes.len() {
                return Err(ParseError::InvalidReference(
                    "finite type edge endpoint".into(),
                ));
            }
            graph.add_edge(
                crate::type_graph::TypeNodeId::new(source),
                crate::type_graph::TypeNodeId::new(target),
                self.type_edge(&fields[2])?,
            );
        }
        let mut limits = GraphLimits::from(self.limits);
        limits.max_work = self.budget.remaining();
        let (graph, used_work) =
            TypeGraph::validate_with_work(graph, constructors, limits).map_err(type_graph_error)?;
        self.charge(used_work)?;
        Ok(Arc::new(graph))
    }

    fn sites(&mut self, value: &Value) -> Result<Vec<SiteRow>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("site table", "array"));
        };
        if values.len() > self.limits.max_sites {
            return Err(ParseError::LimitExceeded("sites"));
        }
        self.list(value, false, |this, value| this.site_row(value))
    }

    fn constructor_replies(
        &mut self,
        value: &Value,
    ) -> Result<Vec<(ConstructorId, ConstructorReply)>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("constructor reply table", "array"));
        };
        if values.len() > self.limits.max_sites {
            return Err(ParseError::LimitExceeded("constructor replies"));
        }
        self.list(value, false, |_, value| {
            let fields = array(value, 2, "constructor reply")?;
            let Value::Array(reply) = &fields[1] else {
                return Err(malformed("constructor reply evidence", "array"));
            };
            let evidence = match reply.as_slice() {
                [tag, node] if unsigned(tag, "constructor reply tag")? == 0 => {
                    ConstructorReply::Static(TypeNodeId(u32_value(node, "reply type node ID")?))
                }
                [tag] if unsigned(tag, "constructor reply tag")? == 1 => ConstructorReply::AtSite,
                [tag, node, field, payload_field, capture_input]
                    if unsigned(tag, "constructor reply tag")? == 2 =>
                {
                    ConstructorReply::StaticWithSite {
                        reply: TypeNodeId(u32_value(node, "reply type node ID")?),
                        field: u32_value(field, "original site field")?,
                        payload_field: u32_value(payload_field, "original payload field")?,
                        capture_input: match capture_input {
                            Value::Null => None,
                            value => Some(u32_value(value, "captured site input")?),
                        },
                    }
                }
                _ => {
                    return Err(ParseError::Malformed(
                        "invalid constructor reply evidence".into(),
                    ))
                }
            };
            Ok((
                ConstructorId(u32_value(&fields[0], "reply constructor ID")?),
                evidence,
            ))
        })
    }

    fn target(&mut self, value: &Value) -> Result<TargetDescriptor, ParseError> {
        let fields = array(value, 6, "target")?;
        let architecture = match unsigned(&fields[0], "architecture")? {
            0 => Architecture::X86_64,
            1 => Architecture::Aarch64,
            tag => return Err(ParseError::InvalidTag(tag)),
        };
        let endianness = match unsigned(&fields[1], "endianness")? {
            0 => Endianness::Little,
            1 => Endianness::Big,
            tag => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(TargetDescriptor {
            architecture,
            endianness,
            pointer_width: u8_value(&fields[2], "pointer width")?,
            word_width: u8_value(&fields[3], "word width")?,
            abi: self.text(&fields[4], "target ABI")?,
            features: self.list(&fields[5], true, |this, value| {
                this.text(value, "target feature")
            })?,
        })
    }

    fn symbol(&mut self, value: &Value) -> Result<SymbolIdentity, ParseError> {
        let fields = array(value, 5, "symbol")?;
        let parent = tagged(&fields[4], "record parent")?;
        let parent_tag = unsigned(&parent[0], "record parent tag")?;
        let record_parent = match (parent_tag, parent.len()) {
            (0, 1) => None,
            (1, 2) => Some(self.text(&parent[1], "record parent")?),
            (0 | 1, _) => return Err(ParseError::Malformed("invalid record parent".into())),
            (tag, _) => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(SymbolIdentity {
            unit: self.text(&fields[0], "symbol unit")?,
            module: self.text(&fields[1], "symbol module")?,
            namespace: self.text(&fields[2], "symbol namespace")?,
            occurrence: self.text(&fields[3], "symbol occurrence")?,
            record_parent,
        })
    }

    fn text(&mut self, value: &Value, what: &str) -> Result<String, ParseError> {
        let text = text_raw(value, what)?;
        self.strings = self
            .strings
            .checked_add(text.len())
            .ok_or(ParseError::LimitExceeded("string bytes"))?;
        if self.strings > self.limits.max_string_bytes {
            return Err(ParseError::LimitExceeded("string bytes"));
        }
        self.charge(text.len())?;
        Ok(text.to_owned())
    }

    fn bytes(&mut self, value: &Value, what: &str) -> Result<Vec<u8>, ParseError> {
        let Value::Bytes(bytes) = value else {
            return Err(malformed(what, "bytes"));
        };
        self.strings = self
            .strings
            .checked_add(bytes.len())
            .ok_or(ParseError::LimitExceeded("string bytes"))?;
        if self.strings > self.limits.max_string_bytes {
            return Err(ParseError::LimitExceeded("string bytes"));
        }
        self.charge(bytes.len())?;
        Ok(bytes.clone())
    }

    fn list<T>(
        &mut self,
        value: &Value,
        table: bool,
        mut decode: impl FnMut(&mut Self, &Value) -> Result<T, ParseError>,
    ) -> Result<Vec<T>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("list", "array"));
        };
        if table {
            self.table(values.len())?;
        } else {
            self.charge(values.len())?;
        }
        self.budget.reserve::<T>(values.len())?;
        values.iter().map(|value| decode(self, value)).collect()
    }

    fn rep(&mut self, value: &Value) -> Result<RuntimeRep, ParseError> {
        let values = tagged(value, "runtime representation")?;
        let tag = unsigned(&values[0], "runtime representation tag")?;
        match (tag, values.len()) {
            (0, 1) => Ok(RuntimeRep::Void),
            (1, 1) => Ok(RuntimeRep::LiftedRef),
            (2, 1) => Ok(RuntimeRep::UnliftedRef),
            (3, 1) => Ok(RuntimeRep::Address),
            (4, 2) => Ok(RuntimeRep::Int(u8_value(&values[1], "integer bits")?)),
            (5, 2) => Ok(RuntimeRep::Word(u8_value(&values[1], "word bits")?)),
            (6, 2) => Ok(RuntimeRep::Float(u8_value(&values[1], "float bits")?)),
            (0..=6, _) => Err(ParseError::Malformed(
                "wrong runtime representation field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn signature(&mut self, value: &Value) -> Result<Signature, ParseError> {
        let fields = array(value, 2, "signature")?;
        Ok(Signature {
            arguments: self.list(&fields[0], false, |this, value| this.rep(value))?,
            results: self.result_contract(&fields[1])?,
        })
    }

    fn result_contract(&mut self, value: &Value) -> Result<super::ResultContract, ParseError> {
        let fields = tagged(value, "result contract")?;
        match (unsigned(&fields[0], "result contract tag")?, fields.len()) {
            (0, 2) => Ok(super::ResultContract::Returns(self.list(
                &fields[1],
                false,
                |this, value| this.rep(value),
            )?)),
            (1, 1) => Ok(super::ResultContract::NoSuccess),
            (2, 1) => Ok(super::ResultContract::CallerResult),
            (0..=2, _) => Err(ParseError::Malformed(
                "wrong result contract field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn field_layout(&mut self, value: &Value) -> Result<FieldLayout, ParseError> {
        let fields = array(value, 2, "field layout")?;
        Ok(FieldLayout {
            rep: self.rep(&fields[0])?,
            offset: u32_value(&fields[1], "field offset")?,
        })
    }

    fn layout(&mut self, value: &Value) -> Result<CheckedLayout, ParseError> {
        let fields = array(value, 4, "checked layout")?;
        Ok(CheckedLayout {
            fields: self.list(&fields[0], false, |this, value| this.field_layout(value))?,
            alignment: u32_value(&fields[1], "layout alignment")?,
            payload_size: u32_value(&fields[2], "layout payload size")?,
            root_mask: self.list(&fields[3], false, |_this, value| {
                bool_value(value, "root mask")
            })?,
        })
    }

    fn constructor(&mut self, value: &Value) -> Result<ConstructorDecl, ParseError> {
        let fields = array(value, 9, "constructor declaration")?;
        Ok(ConstructorDecl {
            identity: self.symbol(&fields[0])?,
            host_id: crate::DataConId(unsigned(&fields[8], "constructor host ID")?),
            family: self.symbol(&fields[1])?,
            field_reps: self.list(&fields[2], false, |this, value| this.rep(value))?,
            strict_fields: self.list(&fields[3], false, |_this, value| {
                bool_value(value, "strict field")
            })?,
            layout: self.layout(&fields[4])?,
            result_rep: self.rep(&fields[5])?,
            tag: u32_value(&fields[6], "constructor tag")?,
            family_size: u32_value(&fields[7], "constructor family size")?,
        })
    }

    fn type_node(
        &mut self,
        value: &Value,
        constructors: &[ConstructorDecl],
    ) -> Result<TypeNode, ParseError> {
        let fields = tagged(value, "finite type node")?;
        let tag = unsigned(&fields[0], "finite type node tag")?;
        match (tag, fields.len()) {
            (0, 4) => Ok(TypeNode::Root {
                domain: match unsigned(&fields[1], "root domain")? {
                    0 => RootDomain::Closed,
                    1 => RootDomain::ConstructorScheme,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
                binders: self.list(&fields[2], false, |_, value| {
                    match unsigned(value, "source binder flag")? {
                        1 => Ok(SourceBinderFlag::Specified),
                        2 => Ok(SourceBinderFlag::Inferred),
                        tag => Err(ParseError::InvalidTag(tag)),
                    }
                })?,
                rendered: self.text(&fields[3], "rendered root type")?,
            }),
            (1, 5) => Ok(TypeNode::Declaration {
                identity: self.symbol(&fields[1])?,
                parameters: self.list(&fields[2], false, |_, value| {
                    match unsigned(value, "declaration parameter flag")? {
                        0 => Ok(ParameterFlag::NamedRequired),
                        1 => Ok(ParameterFlag::NamedSpecified),
                        2 => Ok(ParameterFlag::NamedInferred),
                        3 => Ok(ParameterFlag::AnonymousVisible),
                        tag => Err(ParseError::InvalidTag(tag)),
                    }
                })?,
                form: self.declaration_form(&fields[3])?,
                restriction: match unsigned(&fields[4], "syntax restriction")? {
                    0 => SyntaxRestriction::None,
                    1 => SyntaxRestriction::EffectHead,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
            }),
            (2, 2) => {
                let constructor = ConstructorId(u32_value(&fields[1], "template constructor ID")?);
                let physical = constructors.get(constructor.0 as usize).ok_or_else(|| {
                    ParseError::InvalidReference("template constructor ID".into())
                })?;
                self.budget.charge_symbol_copy(&physical.identity)?;
                let identity = physical.identity.clone();
                Ok(TypeNode::ConstructorTemplate {
                    constructor,
                    identity,
                })
            }
            (3, 2) => Ok(TypeNode::Bound(u32_value(&fields[1], "bound type index")?)),
            (4, 1) => Ok(TypeNode::NominalApplication),
            (5, 1) => Ok(TypeNode::Application),
            (6, 2) => Ok(TypeNode::Function(
                match unsigned(&fields[1], "function flag")? {
                    0 => FunctionFlag::TypeToType,
                    1 => FunctionFlag::TypeToConstraint,
                    2 => FunctionFlag::ConstraintToType,
                    3 => FunctionFlag::ConstraintToConstraint,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
            )),
            (7, 2) => Ok(TypeNode::ForAll(
                match unsigned(&fields[1], "forall flag")? {
                    0 => ForAllFlag::Required,
                    1 => ForAllFlag::Specified,
                    2 => ForAllFlag::Inferred,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
            )),
            (8, 3) => Ok(TypeNode::Literal(
                match unsigned(&fields[1], "type literal kind")? {
                    0 => TypeLiteral::Natural(self.text(&fields[2], "natural type literal")?),
                    1 => TypeLiteral::Symbol(self.text(&fields[2], "symbol type literal")?),
                    2 => TypeLiteral::Character(
                        char::from_u32(u32_value(&fields[2], "character type literal")?)
                            .ok_or_else(|| {
                                ParseError::Malformed("invalid character type literal".into())
                            })?,
                    ),
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
            )),
            (0..=8, _) => Err(ParseError::Malformed(
                "wrong finite type node field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn declaration_form(&mut self, value: &Value) -> Result<DeclarationForm, ParseError> {
        let fields = tagged(value, "declaration form")?;
        let tag = unsigned(&fields[0], "declaration form tag")?;
        match (tag, fields.len()) {
            (0, 1) => Ok(DeclarationForm::Data),
            (1, 2) => Ok(DeclarationForm::Newtype {
                eta_arity: u32_value(&fields[1], "newtype eta arity")?,
            }),
            (2, 1) => Ok(DeclarationForm::Text),
            (3, 1) => Ok(DeclarationForm::Integer),
            (4, 1) => Ok(DeclarationForm::Natural),
            (5, 2) => Ok(DeclarationForm::Scalar(self.rep(&fields[1])?)),
            (6, 3) => Ok(DeclarationForm::Opaque {
                head_kind: match unsigned(&fields[1], "opaque head kind")? {
                    0 => NominalHeadKind::Constructor,
                    1 => NominalHeadKind::Family,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
                reason: self.text(&fields[2], "opaque refusal reason")?,
            }),
            (0..=6, _) => Err(ParseError::Malformed(
                "wrong declaration form field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn type_edge(&mut self, value: &Value) -> Result<TypeEdge, ParseError> {
        let fields = tagged(value, "finite type edge role")?;
        let tag = unsigned(&fields[0], "finite type edge role tag")?;
        let ordinal = |value| u32_value(value, "finite type edge ordinal");
        match (tag, fields.len()) {
            (0, 2) => Ok(TypeEdge::BinderKind(ordinal(&fields[1])?)),
            (1, 1) => Ok(TypeEdge::Body),
            (2, 1) => Ok(TypeEdge::Head),
            (3, 2) => Ok(TypeEdge::Argument(ordinal(&fields[1])?)),
            (4, 1) => Ok(TypeEdge::Function),
            (5, 1) => Ok(TypeEdge::ApplyArgument),
            (6, 1) => Ok(TypeEdge::Multiplicity),
            (7, 1) => Ok(TypeEdge::Domain),
            (8, 1) => Ok(TypeEdge::Codomain),
            (9, 1) => Ok(TypeEdge::Kind),
            (10, 2) => Ok(TypeEdge::Constructor(ordinal(&fields[1])?)),
            (11, 3) => Ok(TypeEdge::Field {
                ordinal: ordinal(&fields[1])?,
                source_rep: self.rep(&fields[2])?,
            }),
            (12, 1) => Ok(TypeEdge::AliasRhs),
            (0..=12, _) => Err(ParseError::Malformed(
                "wrong finite type edge role field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn site_row(&mut self, value: &Value) -> Result<SiteRow, ParseError> {
        let fields = array(value, 6, "site row")?;
        let delivery = match unsigned(&fields[3], "site delivery")? {
            0 => SiteDelivery::HostAnswer,
            1 => SiteDelivery::LiveReentry,
            2 => SiteDelivery::ExitCellFill,
            3 => SiteDelivery::TerminalCapture,
            tag => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(SiteRow {
            site: unsigned(&fields[0], "site ID")?,
            origin: self.text(&fields[1], "site origin")?,
            ordinal: unsigned(&fields[2], "site ordinal")?,
            delivery,
            wire: TypeNodeId(u32_value(&fields[4], "site wire node ID")?),
            inputs: self.list(&fields[5], false, |_, value| {
                Ok(TypeNodeId(u32_value(value, "site input node ID")?))
            })?,
        })
    }

    fn global(&mut self, value: &Value) -> Result<GlobalDecl, ParseError> {
        let fields = array(value, 5, "global declaration")?;
        Ok(GlobalDecl {
            identity: self.symbol(&fields[0])?,
            rep: self.rep(&fields[1])?,
            entry_signature: self.optional_signature(&fields[2])?,
            required_evaluated: bool_value(&fields[3], "required evaluated")?,
            required_generation: self.optional_generation(&fields[4])?,
        })
    }

    fn optional_signature(&mut self, value: &Value) -> Result<Option<SignatureId>, ParseError> {
        let fields = tagged(value, "known entry signature")?;
        match (
            unsigned(&fields[0], "known entry signature tag")?,
            fields.len(),
        ) {
            (0, 1) => Ok(None),
            (1, 2) => Ok(Some(SignatureId(u32_value(
                &fields[1],
                "entry signature ID",
            )?))),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong known entry signature field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn optional_generation(&mut self, value: &Value) -> Result<Option<u64>, ParseError> {
        let fields = tagged(value, "required generation")?;
        match (
            unsigned(&fields[0], "required generation tag")?,
            fields.len(),
        ) {
            (0, 1) => Ok(None),
            (1, 2) => Ok(Some(unsigned(&fields[1], "required generation")?)),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong required generation field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn operation(&mut self, value: &Value) -> Result<OperationDecl, ParseError> {
        let fields = array(value, 2, "operation declaration")?;
        let identity = tagged(&fields[0], "operation identity")?;
        let identity_tag = unsigned(&identity[0], "operation identity tag")?;
        let identity = match (identity_tag, identity.len()) {
            (0, 2) => super::OperationIdentity::PrimOp(self.text(&identity[1], "primop")?),
            (1, 3) => {
                let convention = tagged(&identity[2], "foreign convention")?;
                let convention_tag = unsigned(&convention[0], "foreign convention")?;
                match (convention_tag, convention.len()) {
                    (0, 1) => {}
                    (0, _) => {
                        return Err(ParseError::Malformed("invalid foreign convention".into()));
                    }
                    (tag, _) => return Err(ParseError::InvalidTag(tag)),
                }
                super::OperationIdentity::Intrinsic {
                    symbol: self.text(&identity[1], "intrinsic symbol")?,
                    convention: super::ForeignConvention::CCall,
                }
            }
            (2, 2) => super::OperationIdentity::Capability {
                name: self.text(&identity[1], "capability name")?,
            },
            (3, 2) => {
                let kind_tag = unsigned(&identity[1], "wired-in error kind")?;
                let kind = match kind_tag {
                    0 => super::WiredInErrorKind::PatternMatch,
                    1 => super::WiredInErrorKind::NonExhaustiveGuards,
                    2 => super::WiredInErrorKind::RecordSelector,
                    3 => super::WiredInErrorKind::RecordConstruction,
                    4 => super::WiredInErrorKind::NoMethodBinding,
                    5 => super::WiredInErrorKind::DeferredType,
                    6 => super::WiredInErrorKind::Impossible,
                    7 => super::WiredInErrorKind::ImpossibleConstraint,
                    8 => super::WiredInErrorKind::Absent,
                    9 => super::WiredInErrorKind::AbsentConstraint,
                    10 => super::WiredInErrorKind::AbsentSumField,
                    tag => return Err(ParseError::InvalidTag(tag)),
                };
                super::OperationIdentity::WiredInError { kind }
            }
            (4, 3) => super::OperationIdentity::JsonDecode {
                left: ConstructorId(u32_value(&identity[1], "JSON Left constructor")?),
                right: ConstructorId(u32_value(&identity[2], "JSON Right constructor")?),
            },
            (5, 1) => super::OperationIdentity::JsonEncode,
            (0..=5, _) => {
                return Err(ParseError::Malformed("invalid operation identity".into()));
            }
            (tag, _) => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(OperationDecl {
            identity,
            signature: SignatureId(u32_value(&fields[1], "operation signature ID")?),
        })
    }

    fn json_layout(&mut self, value: &Value) -> Result<super::JsonLayout, ParseError> {
        let fields = array(value, 18, "JSON layout")?;
        let id = |index, label| u32_value(&fields[index], label).map(ConstructorId);
        Ok(super::JsonLayout {
            object: id(0, "JSON Object constructor")?,
            array: id(1, "JSON Array constructor")?,
            string: id(2, "JSON String constructor")?,
            number: id(3, "JSON Number constructor")?,
            bool_: id(4, "JSON Bool constructor")?,
            null: id(5, "JSON Null constructor")?,
            map_bin: id(6, "JSON Map Bin constructor")?,
            map_tip: id(7, "JSON Map Tip constructor")?,
            true_: id(8, "JSON True constructor")?,
            false_: id(9, "JSON False constructor")?,
            cons: id(10, "JSON cons constructor")?,
            nil: id(11, "JSON nil constructor")?,
            scientific: id(12, "JSON Scientific constructor")?,
            integer_small: id(13, "JSON IS constructor")?,
            integer_positive: id(14, "JSON IP constructor")?,
            integer_negative: id(15, "JSON IN constructor")?,
            text: id(16, "JSON Text constructor")?,
            int: id(17, "JSON Int constructor")?,
        })
    }

    fn optional_json_layout(
        &mut self,
        value: &Value,
    ) -> Result<Option<super::JsonLayout>, ParseError> {
        let fields = tagged(value, "program JSON layout")?;
        match (
            unsigned(&fields[0], "program JSON layout tag")?,
            fields.len(),
        ) {
            (0, 1) => Ok(None),
            (1, 2) => self.json_layout(&fields[1]).map(Some),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong program JSON layout field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn value_ref(&mut self, value: &Value) -> Result<ValueRef, ParseError> {
        let fields = tagged(value, "value reference")?;
        if fields.len() != 2 {
            return Err(ParseError::Malformed(
                "wrong value reference field count".into(),
            ));
        }
        match unsigned(&fields[0], "value reference tag")? {
            0 => Ok(ValueRef::Local(ValueId(u32_value(
                &fields[1],
                "local value ID",
            )?))),
            1 => Ok(ValueRef::Global(GlobalId(u32_value(
                &fields[1],
                "global ID",
            )?))),
            tag => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn scalar(&mut self, value: &Value) -> Result<ScalarLiteral, ParseError> {
        let fields = tagged(value, "scalar")?;
        let tag = unsigned(&fields[0], "scalar tag")?;
        match (tag, fields.len()) {
            (0, 3) => Ok(ScalarLiteral::Int {
                bits: u8_value(&fields[1], "integer bits")?,
                bytes: self.bytes(&fields[2], "integer payload")?,
            }),
            (1, 3) => Ok(ScalarLiteral::Word {
                bits: u8_value(&fields[1], "word bits")?,
                bytes: self.bytes(&fields[2], "word payload")?,
            }),
            (2, 3) => Ok(ScalarLiteral::Float {
                bits: u8_value(&fields[1], "float bits")?,
                bytes: self.bytes(&fields[2], "float payload")?,
            }),
            (4, 2) => Ok(ScalarLiteral::Bytes(
                self.bytes(&fields[1], "byte literal")?,
            )),
            (5, 1) => Ok(ScalarLiteral::NullAddress),
            (0..=2 | 4..=5, _) => Err(ParseError::Malformed("wrong scalar field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn atom(&mut self, value: &Value) -> Result<Atom, ParseError> {
        let fields = tagged(value, "atom")?;
        let tag = unsigned(&fields[0], "atom tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(Atom::Ref(self.value_ref(&fields[1])?)),
            (1, 2) => Ok(Atom::Scalar(self.scalar(&fields[1])?)),
            (2, 1) => Ok(Atom::Void),
            (3, 2) => Ok(Atom::Rubbish(self.rep(&fields[1])?)),
            (0..=3, _) => Err(ParseError::Malformed("wrong atom field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn heap_binding<B>(
        &mut self,
        value: &Value,
        body: impl FnMut(&mut Self, &Value) -> Result<B, ParseError>,
    ) -> Result<HeapBinding<B>, ParseError> {
        self.node()?;
        let fields = array(value, 2, "heap binding")?;
        Ok(HeapBinding {
            id: ValueId(u32_value(&fields[0], "heap value ID")?),
            rhs: self.heap_rhs(&fields[1], body)?,
        })
    }

    fn heap_rhs<B>(
        &mut self,
        value: &Value,
        mut body: impl FnMut(&mut Self, &Value) -> Result<B, ParseError>,
    ) -> Result<HeapRhs<B>, ParseError> {
        let fields = tagged(value, "heap RHS")?;
        let tag = unsigned(&fields[0], "heap RHS tag")?;
        match (tag, fields.len()) {
            (0, 5) => Ok(HeapRhs::Function {
                signature: SignatureId(u32_value(&fields[1], "function signature ID")?),
                parameters: self.list(&fields[2], false, |_this, value| {
                    Ok(ValueId(u32_value(value, "parameter ID")?))
                })?,
                captures: self.list(&fields[3], false, |this, value| this.value_ref(value))?,
                body: body(self, &fields[4])?,
            }),
            (1, 5) => Ok(HeapRhs::Thunk {
                signature: SignatureId(u32_value(&fields[1], "thunk signature ID")?),
                update: match unsigned(&fields[2], "update policy")? {
                    0 => UpdatePolicy::Memoize,
                    1 => UpdatePolicy::SingleEntry,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
                captures: self.list(&fields[3], false, |this, value| this.value_ref(value))?,
                body: body(self, &fields[4])?,
            }),
            (2, 3) => Ok(HeapRhs::Constructor {
                constructor: ConstructorId(u32_value(&fields[1], "constructor ID")?),
                fields: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (3, 2) => Ok(HeapRhs::Bytes(self.bytes(&fields[1], "static bytes")?)),
            (0..=3, _) => Err(ParseError::Malformed("wrong heap RHS field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn join_binding(&mut self, value: &Value) -> Result<JoinBinding, ParseError> {
        self.node()?;
        let fields = array(value, 4, "join binding")?;
        Ok(JoinBinding {
            id: JoinId(u32_value(&fields[0], "join ID")?),
            signature: SignatureId(u32_value(&fields[1], "join signature ID")?),
            parameters: self.list(&fields[2], false, |_this, value| {
                Ok(ValueId(u32_value(value, "join parameter ID")?))
            })?,
            body: self.expr_index(&fields[3])?,
        })
    }

    fn pattern(&mut self, value: &Value) -> Result<AlternativePattern, ParseError> {
        let fields = tagged(value, "alternative pattern")?;
        let tag = unsigned(&fields[0], "pattern tag")?;
        match (tag, fields.len()) {
            (0, 1) => Ok(AlternativePattern::Default),
            (1, 2) => Ok(AlternativePattern::Constructor(ConstructorId(u32_value(
                &fields[1],
                "pattern constructor ID",
            )?))),
            (2, 2) => Ok(AlternativePattern::Literal(self.scalar(&fields[1])?)),
            (0..=2, _) => Err(ParseError::Malformed("wrong pattern field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn alternative(&mut self, value: &Value) -> Result<Alternative, ParseError> {
        self.node()?;
        let fields = array(value, 3, "alternative")?;
        Ok(Alternative {
            pattern: self.pattern(&fields[0])?,
            binders: self.list(&fields[1], false, |_this, value| {
                Ok(ValueId(u32_value(value, "alternative binder ID")?))
            })?,
            body: self.expr_index(&fields[2])?,
        })
    }

    fn case_kind(&mut self, value: &Value) -> Result<CaseKind, ParseError> {
        let fields = tagged(value, "case kind")?;
        let tag = unsigned(&fields[0], "case kind tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(CaseKind::Algebraic(self.symbol(&fields[1])?)),
            (1, 2) => Ok(CaseKind::Primitive(self.rep(&fields[1])?)),
            (2, 1) => Ok(CaseKind::MultiValue),
            (3, 1) => Ok(CaseKind::Polymorphic),
            (0..=3, _) => Err(ParseError::Malformed("wrong case kind field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn expr(&mut self, value: &Value) -> Result<Expr, ParseError> {
        Ok(Expr {
            nodes: self.list(value, false, Self::expr_frame)?,
        })
    }

    fn expr_index(&mut self, value: &Value) -> Result<usize, ParseError> {
        usize::try_from(unsigned(value, "expression index")?)
            .map_err(|_| malformed("expression index", "usize"))
    }

    fn expr_frame(&mut self, value: &Value) -> Result<ExprFrame<usize>, ParseError> {
        self.node()?;
        let fields = tagged(value, "expression")?;
        let tag = unsigned(&fields[0], "expression tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(ExprFrame::Return(self.list(
                &fields[1],
                false,
                |this, value| this.atom(value),
            )?)),
            (1, 3) => Ok(ExprFrame::Enter {
                callee: self.atom(&fields[1])?,
                signature: SignatureId(u32_value(&fields[2], "enter signature ID")?),
            }),
            (2, 4) => Ok(ExprFrame::Call {
                callee: self.atom(&fields[1])?,
                signature: SignatureId(u32_value(&fields[2], "call signature ID")?),
                arguments: self.list(&fields[3], false, |this, value| this.atom(value))?,
            }),
            (3, 3) => Ok(ExprFrame::Operation {
                operation: OperationId(u32_value(&fields[1], "operation ID")?),
                arguments: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (4, 3) => Ok(ExprFrame::Construct {
                constructor: ConstructorId(u32_value(&fields[1], "constructor ID")?),
                fields: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (5, 6) => Ok(ExprFrame::Case {
                scrutinee: self.expr_index(&fields[1])?,
                binder: ValueId(u32_value(&fields[2], "case binder ID")?),
                scrutinee_results: self.result_contract(&fields[3])?,
                kind: self.case_kind(&fields[4])?,
                alternatives: self
                    .list(&fields[5], false, |this, value| this.alternative(value))?,
            }),
            (6, 3) => Ok(ExprFrame::Let {
                bindings: self.heap_group(&fields[1])?,
                body: self.expr_index(&fields[2])?,
            }),
            (7, 3) => Ok(ExprFrame::LetJoins {
                bindings: self.join_group(&fields[1])?,
                body: self.expr_index(&fields[2])?,
            }),
            (8, 3) => Ok(ExprFrame::Jump {
                join: JoinId(u32_value(&fields[1], "jump join ID")?),
                arguments: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (0..=8, _) => Err(ParseError::Malformed("wrong expression field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn heap_group(&mut self, value: &Value) -> Result<Group<HeapBinding<usize>>, ParseError> {
        self.group(value, |this, value| {
            this.heap_binding(value, Self::expr_index)
        })
    }

    fn join_group(&mut self, value: &Value) -> Result<Group<JoinBinding>, ParseError> {
        self.group(value, |this, value| this.join_binding(value))
    }

    fn top_group(&mut self, value: &Value) -> Result<Group<TopBinding>, ParseError> {
        self.group(value, |this, value| {
            this.node()?;
            let fields = array(value, 2, "top binding")?;
            Ok(TopBinding {
                identity: this.symbol(&fields[0])?,
                binding: this.heap_binding(&fields[1], Self::expr_index)?,
            })
        })
    }

    fn group<T>(
        &mut self,
        value: &Value,
        mut decode: impl FnMut(&mut Self, &Value) -> Result<T, ParseError>,
    ) -> Result<Group<T>, ParseError> {
        let fields = tagged(value, "binding group")?;
        let tag = unsigned(&fields[0], "binding group tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(Group::NonRecursive(decode(self, &fields[1])?)),
            (1, 2) => Ok(Group::Recursive(self.list(
                &fields[1],
                false,
                |this, value| decode(this, value),
            )?)),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong binding group field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }
}

fn array<'a>(value: &'a Value, length: usize, what: &str) -> Result<&'a [Value], ParseError> {
    let Value::Array(values) = value else {
        return Err(malformed(what, "array"));
    };
    if values.len() != length {
        return Err(ParseError::Malformed(format!(
            "{what} has {} fields, expected {length}",
            values.len()
        )));
    }
    Ok(values)
}

fn tagged<'a>(value: &'a Value, what: &str) -> Result<&'a [Value], ParseError> {
    let Value::Array(values) = value else {
        return Err(malformed(what, "tagged array"));
    };
    if values.is_empty() {
        return Err(ParseError::Malformed(format!("{what} is empty")));
    }
    Ok(values)
}

fn unsigned(value: &Value, what: &str) -> Result<u64, ParseError> {
    let Value::Integer(integer) = value else {
        return Err(malformed(what, "unsigned integer"));
    };
    u64::try_from(*integer).map_err(|_| malformed(what, "unsigned integer"))
}

fn u32_value(value: &Value, what: &str) -> Result<u32, ParseError> {
    u32::try_from(unsigned(value, what)?).map_err(|_| malformed(what, "u32"))
}

fn u8_value(value: &Value, what: &str) -> Result<u8, ParseError> {
    u8::try_from(unsigned(value, what)?).map_err(|_| malformed(what, "u8"))
}

fn bool_value(value: &Value, what: &str) -> Result<bool, ParseError> {
    let Value::Bool(value) = value else {
        return Err(malformed(what, "boolean"));
    };
    Ok(*value)
}

fn text_raw<'a>(value: &'a Value, what: &str) -> Result<&'a str, ParseError> {
    let Value::Text(value) = value else {
        return Err(malformed(what, "text"));
    };
    Ok(value)
}

fn malformed(what: &str, expected: &str) -> ParseError {
    ParseError::Malformed(format!("{what} must be {expected}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_validation_work_remains_charged_before_later_tables() {
        let graph = super::super::testing::closed_type_graph(
            super::super::testing::identity("Types", "Text"),
            DeclarationForm::Text,
        );
        let (_, used_work) =
            TypeGraph::validate_with_work(graph.graph().clone(), &[], GraphLimits::default())
                .unwrap();
        let mut budget = OperationBudget::new(DecodeLimits::default().max_work);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        decoder
            .type_graph(&encode_type_graph_value(&graph), &[])
            .unwrap();
        assert!(decoder.budget.spent() >= used_work);
        decoder.budget.charge(decoder.budget.remaining()).unwrap();
        assert_eq!(
            decoder.sites(&Value::Array(vec![Value::Array(vec![])])),
            Err(ParseError::LimitExceeded("work"))
        );
    }

    fn deep_program(depth: usize) -> Vec<u8> {
        let array = Value::Array;
        let n = |value: usize| Value::Integer((value as u64).into());
        let text = |value: &str| Value::Text(value.into());
        let rep = || array(vec![n(4), n(64)]);
        let leaf = || {
            array(vec![
                n(0),
                array(vec![array(vec![
                    n(1),
                    array(vec![
                        n(0),
                        n(64),
                        Value::Bytes(1_i64.to_be_bytes().to_vec()),
                    ]),
                ])]),
            ])
        };
        let mut nodes = vec![leaf()];
        for level in 1..=depth {
            let body = nodes.len() - 1;
            let scrutinee = nodes.len();
            nodes.push(leaf());
            nodes.push(array(vec![
                n(5),
                n(scrutinee),
                n(level),
                array(vec![n(0), array(vec![rep()])]),
                array(vec![n(1), rep()]),
                array(vec![array(vec![array(vec![n(0)]), array(vec![]), n(body)])]),
            ]));
        }
        let root = nodes.len() - 1;
        let wire = array(vec![
            text("TPSTG"),
            n(super::super::SCHEMA_VERSION as usize),
            text("ghc-9.12-prepared-stg"),
            text("ghc-9.12.2"),
            n(super::super::EXECUTION_ABI_VERSION as usize),
            array(vec![
                n(0),
                n(0),
                n(64),
                n(64),
                text("sysv64"),
                array(vec![]),
            ]),
            array(vec![array(vec![
                array(vec![]),
                array(vec![n(0), array(vec![rep()])]),
            ])]),
            array(vec![]),
            array(vec![]),
            array(vec![]),
            array(nodes),
            array(vec![array(vec![
                n(0),
                array(vec![
                    array(vec![
                        text("deep"),
                        text("Fixture"),
                        text("value"),
                        text("entry"),
                        array(vec![n(0)]),
                    ]),
                    array(vec![
                        n(0),
                        array(vec![n(0), n(0), array(vec![]), array(vec![]), n(root)]),
                    ]),
                ]),
            ])]),
            n(0),
            encode_type_graph_value(&TypeGraph::default()),
            array(vec![]),
            array(vec![]),
            array(vec![n(0)]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&wire, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn deep_flat_program_is_stack_safe_through_decode_validation_and_drop() {
        const CHILD: &str = "TIDEPOOL_DEEP_FLAT_SCHEMA_CHILD";
        if std::env::var_os(CHILD).is_none() {
            #[allow(
                clippy::disallowed_methods,
                reason = "test fixture: re-execs this test binary itself to exercise a bounded stack, not a production launch site"
            )]
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution_schema::codec::tests::deep_flat_program_is_stack_safe_through_decode_validation_and_drop", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let bytes = deep_program(20_000);
                let wire = decode_wire(
                    &bytes,
                    DecodeLimits::default(),
                    &mut OperationBudget::new(DecodeLimits::default().max_work),
                )
                .unwrap();
                let requirements = super::super::ProgramRequirements {
                    schema_version: super::super::SCHEMA_VERSION,
                    projection_profile: wire.envelope.projection_profile.clone(),
                    toolchain: wire.envelope.toolchain.clone(),
                    execution_abi_version: super::super::EXECUTION_ABI_VERSION,
                    target: wire.envelope.target.clone(),
                };
                drop(wire);
                let prepared =
                    super::super::parse_program(&bytes, &requirements, DecodeLimits::default())
                        .unwrap();
                assert_eq!(prepared.expressions().nodes.len(), 40_001);
                let cloned = prepared.clone();
                assert_eq!(prepared, cloned);
                assert!(!format!("{prepared:?}").is_empty());
                drop((prepared, cloned));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn malformed_nested_cbor_is_rejected_before_recursive_materialization() {
        let mut bytes = vec![0x81; 100_000];
        bytes.push(0);
        assert!(matches!(
            decode_wire(
                &bytes,
                DecodeLimits::default(),
                &mut OperationBudget::new(DecodeLimits::default().max_work)
            ),
            Err(ParseError::Malformed(_))
        ));
        assert!(matches!(
            scan_item(&[0x9f, 0xff], &mut OperationBudget::new(10)),
            Err(ParseError::Malformed(_))
        ));
        assert!(matches!(
            scan_item(&[0x82, 0], &mut OperationBudget::new(10)),
            Err(ParseError::Truncated)
        ));
        assert!(matches!(
            scan_item(&[0x82, 0, 0], &mut OperationBudget::new(2)),
            Err(ParseError::LimitExceeded("work"))
        ));
    }

    fn number(value: u8) -> Value {
        Value::Integer(value.into())
    }

    #[test]
    fn null_address_and_managed_rubbish_have_distinct_wire_forms() {
        let mut budget = OperationBudget::new(DecodeLimits::default().max_work);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        let null = Value::Array(vec![number(1), Value::Array(vec![number(5)])]);
        let rubbish = Value::Array(vec![number(3), Value::Array(vec![number(1)])]);
        assert_eq!(
            decoder.atom(&null).unwrap(),
            Atom::Scalar(ScalarLiteral::NullAddress)
        );
        assert_eq!(
            decoder.atom(&rubbish).unwrap(),
            Atom::Rubbish(RuntimeRep::LiftedRef)
        );
        // A literal pattern cannot smuggle an atom through the scalar grammar.
        assert!(decoder.scalar(&rubbish).is_err());
    }

    #[test]
    fn retired_character_scalar_tag_is_not_decoded() {
        let mut budget = OperationBudget::new(DecodeLimits::default().max_work);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        assert!(matches!(
            decoder.scalar(&Value::Array(vec![number(3), number(65)])),
            Err(ParseError::InvalidTag(3))
        ));
    }

    #[test]
    fn rubbish_wire_representation_is_explicit_and_required() {
        let mut budget = OperationBudget::new(DecodeLimits::default().max_work);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        for (wire_rep, rep) in [
            (vec![number(2)], RuntimeRep::UnliftedRef),
            (vec![number(3)], RuntimeRep::Address),
            (vec![number(4), number(64)], RuntimeRep::Int(64)),
            (vec![number(6), number(32)], RuntimeRep::Float(32)),
        ] {
            let atom = Value::Array(vec![number(3), Value::Array(wire_rep)]);
            assert_eq!(decoder.atom(&atom).unwrap(), Atom::Rubbish(rep));
        }
        assert!(decoder.atom(&Value::Array(vec![number(3)])).is_err());
        assert!(decoder
            .atom(&Value::Array(vec![number(3), number(1)]))
            .is_err());
    }

    #[test]
    fn json_layout_codec_requires_all_named_roles() {
        let mut budget = OperationBudget::new(DecodeLimits::default().max_work);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        let complete = Value::Array((0_u8..18).map(number).collect());
        let layout = decoder.json_layout(&complete).unwrap();
        assert_eq!(layout.object, ConstructorId(0));
        assert_eq!(layout.int, ConstructorId(17));

        let short = Value::Array((0_u8..17).map(number).collect());
        assert!(matches!(
            decoder.json_layout(&short),
            Err(ParseError::Malformed(_))
        ));
    }
}

#[cfg(test)]
mod operation_budget_tests {
    use super::super::ProgramRequirements;
    use super::*;

    fn fixture() -> Vec<u8> {
        tidepool_test_data::prepared_encode::encode_wire_program(
            &tidepool_test_data::prepared::text_type_program(),
        )
    }

    #[test]
    fn byte_entry_budget_spans_raw_decode_graph_freeze_and_semantic_validation() {
        let bytes = fixture();
        let limits = DecodeLimits::default();
        let mut raw = OperationBudget::new(limits.max_work);
        let value = decode_value(&bytes, limits, &mut raw).unwrap();
        let mut typed = OperationBudget::new(limits.max_work);
        let wire = Decoder::new(limits, &mut typed).program(&value).unwrap();
        let requirements = ProgramRequirements {
            schema_version: wire.envelope.schema_version,
            projection_profile: wire.envelope.projection_profile.clone(),
            toolchain: wire.envelope.toolchain.clone(),
            execution_abi_version: wire.envelope.execution_abi_version,
            target: wire.envelope.target.clone(),
        };
        let mut semantic = OperationBudget::new(limits.max_work);
        super::super::validation::validate_program_with_budget(
            &wire,
            &requirements,
            limits,
            &mut semantic,
        )
        .unwrap();
        let phase_limit = raw.spent().max(typed.spent()).max(semantic.spent());
        let total = raw.spent() + typed.spent() + semantic.spent();
        assert!(total > phase_limit);
        let per_phase = DecodeLimits {
            max_work: phase_limit,
            ..limits
        };
        decode_value(&bytes, per_phase, &mut OperationBudget::new(phase_limit)).unwrap();
        Decoder::new(per_phase, &mut OperationBudget::new(phase_limit))
            .program(&value)
            .unwrap();
        super::super::validation::validate_program(&wire, &requirements, per_phase).unwrap();
        assert_eq!(
            super::super::parse_program(&bytes, &requirements, per_phase),
            Err(ParseError::LimitExceeded("work")),
        );
        super::super::parse_program(
            &bytes,
            &requirements,
            DecodeLimits {
                max_work: total,
                ..limits
            },
        )
        .unwrap();
    }

    #[test]
    fn physical_template_identity_copy_is_admitted_before_clone() {
        let bytes = tidepool_test_data::prepared_encode::encode_wire_program(
            &tidepool_test_data::prepared::constructor_program(),
        );
        let limits = DecodeLimits::default();
        let mut budget = OperationBudget::new(limits.max_work);
        let value = decode_value(&bytes, limits, &mut budget).unwrap();
        let mut constructors = Decoder::new(limits, &mut budget)
            .program(&value)
            .unwrap()
            .constructors;
        constructors[0].identity.occurrence = "X".repeat(4096);
        let identity = &constructors[0].identity;
        let copy_bytes = [
            &identity.unit,
            &identity.module,
            &identity.namespace,
            &identity.occurrence,
        ]
        .into_iter()
        .chain(identity.record_parent.iter())
        .map(|text| text.len())
        .sum::<usize>();
        let template = Value::Array(vec![Value::Integer(2.into()), Value::Integer(0.into())]);
        let mut budget = OperationBudget::new(copy_bytes - 1);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        assert_eq!(
            decoder.type_node(&template, &constructors),
            Err(ParseError::LimitExceeded("work"))
        );
        assert_eq!(decoder.budget.spent(), 0);
        let mut budget = OperationBudget::new(copy_bytes);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        assert!(
            matches!(decoder.type_node(&template, &constructors).unwrap(),
            TypeNode::ConstructorTemplate { identity: copied, .. } if copied == *identity)
        );
        assert_eq!(decoder.budget.spent(), copy_bytes);

        // The second physical ID refuses before it could consume another copy.
        let duplicate = Value::Array(vec![
            Value::Integer(1.into()),
            Value::Array(vec![template.clone(), template]),
            Value::Array(vec![]),
        ]);
        let mut budget = OperationBudget::new(copy_bytes + 2);
        let mut decoder = Decoder::new(DecodeLimits::default(), &mut budget);
        assert_eq!(
            decoder.type_graph(&duplicate, &constructors),
            Err(ParseError::DuplicateDefinition(
                "type constructor template".into()
            ))
        );
        assert_eq!(decoder.budget.spent(), copy_bytes + 2);
    }
}
