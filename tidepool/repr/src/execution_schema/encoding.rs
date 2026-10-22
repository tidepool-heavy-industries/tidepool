//! Canonical prepared-data encoding. Encoding never issues compiler evidence.
//!
//! The pinned CBOR encoder writes borrowed fields directly. Keep matches and
//! struct destructuring exhaustive when changing the prepared schema.

use super::*;
use ciborium_io::Write;
use ciborium_ll::{Encoder as CborEncoder, Header};
use sha2::{Digest, Sha256};

// Flat arenas keep record nesting independent of program depth. Emit each
// expression before visiting the next; never build an intermediate CBOR tree.
macro_rules! array {
    (@one $item:expr) => { () };
    ($out:expr; $($item:expr),* $(,)?) => {{
        let length = <[()]>::len(&[$(array!(@one $item)),*]);
        $out.push(Header::Array(Some(length)))?;
        $( $item?; )*
        Ok::<(), ParseError>(())
    }};
}

enum Destination<'a> {
    Bytes(&'a mut Vec<u8>),
    Digest(&'a mut Sha256),
}
struct Sink<'a> {
    destination: Destination<'a>,
    budget: Option<&'a mut OperationBudget>,
}
impl Write for Sink<'_> {
    type Error = ParseError;
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), ParseError> {
        if let Some(budget) = &mut self.budget {
            budget.charge(bytes.len())?;
        }
        match &mut self.destination {
            Destination::Bytes(output) => output.extend_from_slice(bytes),
            Destination::Digest(hash) => hash.update(bytes),
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), ParseError> {
        Ok(())
    }
}
type Encoder<'a> = CborEncoder<Sink<'a>>;

fn n(value: impl Into<u64>, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    out.push(Header::Positive(value.into()))
}
fn s(value: &str, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    out.text(value, None)
}
fn binary(value: &[u8], out: &mut Encoder<'_>) -> Result<(), ParseError> {
    out.bytes(value, None)
}
fn boolean(value: bool, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    out.push(Header::Simple(if value {
        ciborium_ll::simple::TRUE
    } else {
        ciborium_ll::simple::FALSE
    }))
}
fn null(out: &mut Encoder<'_>) -> Result<(), ParseError> {
    out.push(Header::Simple(ciborium_ll::simple::NULL))
}
fn list<T>(
    values: &[T],
    encode: impl Fn(&T, &mut Encoder<'_>) -> Result<(), ParseError>,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    out.push(Header::Array(Some(values.len())))?;
    for value in values {
        encode(value, out)?;
    }
    Ok(())
}
fn option<T>(
    value: &Option<T>,
    encode: impl Fn(&T, &mut Encoder<'_>) -> Result<(), ParseError>,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    match value {
        None => array!(out; n(0_u64, out)),
        Some(value) => array!(out; n(1_u64, out), encode(value, out)),
    }
}
fn bytes(encode: impl FnOnce(&mut Encoder<'_>) -> Result<(), ParseError>) -> Vec<u8> {
    let mut output = Vec::new();
    encode(&mut Encoder::from(Sink {
        destination: Destination::Bytes(&mut output),
        budget: None,
    }))
    .expect("unbounded in-memory prepared encoding");
    output
}

/// Encode current typed program data without validating or issuing authority.
pub fn encode_wire_program(wire: &WireProgram) -> Vec<u8> {
    bytes(|out| {
        let WireProgram {
            envelope,
            signatures,
            globals,
            constructors,
            operations,
            expressions,
            bindings,
            entry,
            types,
            sites,
            constructor_replies,
            json_layout,
        } = wire;
        out.push(Header::Array(Some(17)))?;
        s("TPSTG", out)?;
        definitions(
            envelope,
            signatures,
            globals,
            constructors,
            operations,
            expressions,
            bindings,
            Some(*entry),
            types,
            sites,
            constructor_replies,
            json_layout,
            out,
        )
    })
}

/// Encode current typed module products without issuing compiler evidence.
pub fn encode_module_products(products: &[RawModuleProduct]) -> Vec<u8> {
    bytes(|out| {
        array!(out;
            s("TPMOD", out), n(MODULE_PRODUCTS_VERSION, out),
            list(products, |product, out| {
                let RawModuleProduct { unit, module, interface, groups } = product;
                array!(out; s(unit, out), s(module, out), binary(interface, out),
                    list(groups, |group, out| binary(&encode_projected_group(group), out), out))
            }, out)
        )
    })
}

/// Encode an entry-free group without issuing compiler provenance.
pub fn encode_projected_group(group: &ProjectedGroup) -> Vec<u8> {
    bytes(|out| projected_group(group, out))
}

/// Hash exact prepared-group bytes without copying definitions or materializing
/// encoded data. Charge before each digest write; refusal stops traversal and
/// never yields a partial commitment.
pub fn hash_projected_group(
    group: &ProjectedGroup,
    operation: &InventoryOperation,
) -> Result<[u8; 32], ParseError> {
    let mut hash = Sha256::new();
    operation.with_budget(|budget| {
        projected_group(
            group,
            &mut Encoder::from(Sink {
                destination: Destination::Digest(&mut hash),
                budget: Some(budget),
            }),
        )
    })?;
    Ok(hash.finalize().into())
}

fn projected_group(group: &ProjectedGroup, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let content = group.definitions();
    out.push(Header::Array(Some(19)))?;
    s("TPGRP", out)?;
    n(2_u64, out)?;
    n(group.original_ordinal(), out)?;
    list(group.binders(), symbol, out)?;
    definitions(
        content.envelope(),
        content.signatures(),
        content.globals(),
        content.constructors(),
        content.operations(),
        content.expressions(),
        content.bindings(),
        None,
        content.types(),
        content.sites(),
        content.constructor_replies(),
        &content.json_layout().copied(),
        out,
    )
}

#[allow(clippy::too_many_arguments)]
fn definitions(
    envelope: &ProgramEnvelope,
    signatures: &[Signature],
    globals: &[GlobalDecl],
    constructors: &[ConstructorDecl],
    operations: &[OperationDecl],
    expressions: &Expr,
    bindings: &[Group<TopBinding>],
    entry: Option<ValueId>,
    types: &crate::type_graph::TypeGraph,
    sites: &[SiteRow],
    constructor_replies: &[(ConstructorId, ConstructorReply)],
    json_layout: &Option<JsonLayout>,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    let ProgramEnvelope {
        schema_version,
        projection_profile,
        toolchain,
        execution_abi_version,
        target,
    } = envelope;
    let Expr { nodes } = expressions;
    n(*schema_version, out)?;
    s(projection_profile, out)?;
    s(toolchain, out)?;
    n(*execution_abi_version, out)?;
    target_value(target, out)?;
    list(signatures, signature, out)?;
    list(globals, global, out)?;
    list(constructors, constructor, out)?;
    list(operations, operation, out)?;
    list(nodes, expression, out)?;
    list(bindings, |value, out| group(value, top, out), out)?;
    if let Some(entry) = entry {
        n(entry.0, out)?;
    }
    type_graph(types, out)?;
    list(sites, site, out)?;
    list(
        constructor_replies,
        |(id, reply), out| {
            array!(out; n(id.0, out), match reply {
                ConstructorReply::Static(node) => array!(out; n(0_u64, out), n(node.0, out)),
                ConstructorReply::AtSite => array!(out; n(1_u64, out)),
                ConstructorReply::StaticWithSite { reply, field, payload_field, capture_input } => {
                    array!(out; n(2_u64, out), n(reply.0, out), n(*field, out), n(*payload_field, out),
                        match capture_input { Some(input) => n(*input, out), None => null(out) })
                }
            })
        },
        out,
    )?;
    option(json_layout, json, out)
}

fn target_value(value: &TargetDescriptor, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let TargetDescriptor {
        architecture,
        endianness,
        pointer_width,
        word_width,
        abi,
        features,
    } = value;
    array!(out;
        n(match architecture {
            Architecture::X86_64 => 0_u64,
            Architecture::Aarch64 => 1,
        }, out),
        n(match endianness {
            Endianness::Little => 0_u64,
            Endianness::Big => 1,
        }, out),
        n(*pointer_width, out),
        n(*word_width, out),
        s(abi, out),
        list(features, |value, out| s(value, out), out),
    )
}
fn symbol(value: &SymbolIdentity, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let SymbolIdentity {
        unit,
        module,
        namespace,
        occurrence,
        record_parent,
    } = value;
    array!(out;
        s(unit, out),
        s(module, out),
        s(namespace, out),
        s(occurrence, out),
        option(record_parent, |value, out| s(value, out), out),
    )
}
fn type_identity_value(value: &TypeIdentity, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    array!(out;
        symbol(&value.symbol, out),
        match &value.version {
            TypeVersion::WiredIn => array!(out;n(0_u64, out)),
            TypeVersion::GhcAbi(bytes) => array!(out;n(1_u64, out), binary(bytes, out)),
        },
    )
}
fn constructor_identity_value(
    value: &ConstructorIdentity,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    array!(out;symbol(&value.symbol, out), type_identity_value(&value.family, out))
}

fn rep(value: &RuntimeRep, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        RuntimeRep::Void => array!(out;n(0_u64, out)),
        RuntimeRep::LiftedRef => array!(out;n(1_u64, out)),
        RuntimeRep::UnliftedRef => array!(out;n(2_u64, out)),
        RuntimeRep::Address => array!(out;n(3_u64, out)),
        RuntimeRep::Int(bits) => array!(out;n(4_u64, out), n(*bits, out)),
        RuntimeRep::Word(bits) => array!(out;n(5_u64, out), n(*bits, out)),
        RuntimeRep::Float(bits) => array!(out;n(6_u64, out), n(*bits, out)),
    }
}
fn result(value: &ResultContract, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        ResultContract::Returns(reps) => array!(out;n(0_u64, out), list(reps, rep, out)),
        ResultContract::NoSuccess => array!(out;n(1_u64, out)),
        ResultContract::CallerResult => array!(out;n(2_u64, out)),
    }
}
fn signature(value: &Signature, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let Signature { arguments, results } = value;
    array!(out;list(arguments, rep, out), result(results, out))
}
fn layout(value: &CheckedLayout, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let CheckedLayout {
        fields,
        alignment,
        payload_size,
        root_mask,
    } = value;
    array!(out;
        list(fields, |value, out| {
            let FieldLayout {
                rep: representation,
                offset,
            } = value;
            array!(out;rep(representation, out), n(*offset, out))
        }, out),
        n(*alignment, out),
        n(*payload_size, out),
        list(root_mask, |value, out| boolean(*value, out), out),
    )
}
fn constructor(value: &ConstructorDecl, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let ConstructorDecl {
        identity,
        host_id,
        result_rep,
        field_reps,
        strict_fields,
        layout: storage,
        tag,
        family_size,
    } = value;
    array!(out;
        constructor_identity_value(identity, out),
        list(field_reps, rep, out),
        list(strict_fields, |value, out| boolean(*value, out), out),
        layout(storage, out),
        rep(result_rep, out),
        n(*tag, out),
        n(*family_size, out),
        n(host_id.0, out),
    )
}
fn site(value: &SiteRow, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let SiteRow {
        site,
        origin,
        ordinal,
        delivery,
        wire,
        inputs,
    } = value;
    array!(out;
        n(*site, out),
        s(origin, out),
        n(*ordinal, out),
        n(match delivery {
            SiteDelivery::HostAnswer => 0_u64,
            SiteDelivery::LiveReentry => 1,
            SiteDelivery::ExitCellFill => 2,
            SiteDelivery::TerminalCapture => 3,
        }, out),
        n(wire.0, out),
        list(inputs, |id, out| n(id.0, out), out),
    )
}
fn global(value: &GlobalDecl, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let GlobalDecl {
        identity,
        rep: representation,
        entry_signature,
        required_evaluated,
        required_generation,
    } = value;
    array!(out;
        symbol(identity, out),
        rep(representation, out),
        option(entry_signature, |id, out| n(id.0, out), out),
        boolean(*required_evaluated, out),
        option(required_generation, |value, out| n(*value, out), out),
    )
}
fn operation(value: &OperationDecl, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let OperationDecl {
        identity,
        signature,
    } = value;
    out.push(Header::Array(Some(2)))?;
    match identity {
        OperationIdentity::PrimOp(name) => array!(out;n(0_u64, out), s(name, out)),
        OperationIdentity::Intrinsic { symbol, convention } => array!(out;
            n(1_u64, out),
            s(symbol, out),
            array!(out;n(match convention {
                ForeignConvention::CCall => 0_u64,
            }, out)),
        ),
        OperationIdentity::Capability { name } => array!(out;n(2_u64, out), s(name, out)),
        OperationIdentity::WiredInError { kind } => array!(out;
            n(3_u64, out),
            n(match kind {
                WiredInErrorKind::PatternMatch => 0_u64,
                WiredInErrorKind::NonExhaustiveGuards => 1,
                WiredInErrorKind::RecordSelector => 2,
                WiredInErrorKind::RecordConstruction => 3,
                WiredInErrorKind::NoMethodBinding => 4,
                WiredInErrorKind::DeferredType => 5,
                WiredInErrorKind::Impossible => 6,
                WiredInErrorKind::ImpossibleConstraint => 7,
                WiredInErrorKind::Absent => 8,
                WiredInErrorKind::AbsentConstraint => 9,
                WiredInErrorKind::AbsentSumField => 10,
            }, out),
        ),
        OperationIdentity::JsonDecode { left, right } => {
            array!(out;n(4_u64, out), n(left.0, out), n(right.0, out))
        }
        OperationIdentity::JsonEncode => array!(out;n(5_u64, out)),
    }?;
    n(signature.0, out)
}
fn json(value: &JsonLayout, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let JsonLayout {
        object,
        array,
        string,
        number,
        bool_,
        null,
        map_bin,
        map_tip,
        true_,
        false_,
        cons,
        nil,
        scientific,
        integer_small,
        integer_positive,
        integer_negative,
        text,
        int,
    } = value;
    list(
        &[
            object,
            array,
            string,
            number,
            bool_,
            null,
            map_bin,
            map_tip,
            true_,
            false_,
            cons,
            nil,
            scientific,
            integer_small,
            integer_positive,
            integer_negative,
            text,
            int,
        ],
        |id, out| n(id.0, out),
        out,
    )
}
fn reference(value: &ValueRef, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        ValueRef::Local(id) => array!(out;n(0_u64, out), n(id.0, out)),
        ValueRef::Global(id) => array!(out;n(1_u64, out), n(id.0, out)),
    }
}
fn scalar(value: &ScalarLiteral, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        ScalarLiteral::Int { bits, bytes } => {
            array!(out;n(0_u64, out), n(*bits, out), binary(bytes, out))
        }
        ScalarLiteral::Word { bits, bytes } => {
            array!(out;n(1_u64, out), n(*bits, out), binary(bytes, out))
        }
        ScalarLiteral::Float { bits, bytes } => {
            array!(out;n(2_u64, out), n(*bits, out), binary(bytes, out))
        }
        ScalarLiteral::Bytes(bytes) => array!(out;n(4_u64, out), binary(bytes, out)),
        ScalarLiteral::NullAddress => array!(out;n(5_u64, out)),
    }
}
fn atom(value: &Atom, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        Atom::Ref(value) => array!(out;n(0_u64, out), reference(value, out)),
        Atom::Scalar(value) => array!(out;n(1_u64, out), scalar(value, out)),
        Atom::Void => array!(out;n(2_u64, out)),
        Atom::Rubbish(value) => array!(out;n(3_u64, out), rep(value, out)),
    }
}
fn group<T>(
    value: &Group<T>,
    encode: impl Fn(&T, &mut Encoder<'_>) -> Result<(), ParseError>,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    match value {
        Group::NonRecursive(value) => array!(out;n(0_u64, out), encode(value, out)),
        Group::Recursive(values) => array!(out;n(1_u64, out), list(values, encode, out)),
    }
}
fn heap(value: &HeapBinding, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let HeapBinding { id, rhs } = value;
    out.push(Header::Array(Some(2)))?;
    n(id.0, out)?;
    match rhs {
        HeapRhs::Function {
            signature,
            parameters,
            captures,
            body,
        } => array!(out;
            n(0_u64, out),
            n(signature.0, out),
            list(parameters, |id, out| n(id.0, out), out),
            list(captures, reference, out),
            n(*body as u64, out),
        ),
        HeapRhs::Thunk {
            signature,
            update,
            captures,
            body,
        } => array!(out;
            n(1_u64, out),
            n(signature.0, out),
            n(match update {
                UpdatePolicy::Memoize => 0_u64,
                UpdatePolicy::SingleEntry => 1,
            }, out),
            list(captures, reference, out),
            n(*body as u64, out),
        ),
        HeapRhs::Constructor {
            constructor,
            fields,
        } => array!(out;n(2_u64, out), n(constructor.0, out), list(fields, atom, out)),
        HeapRhs::Bytes(bytes) => array!(out;n(3_u64, out), binary(bytes, out)),
    }
}
fn top(value: &TopBinding, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let TopBinding { identity, binding } = value;
    array!(out;symbol(identity, out), heap(binding, out))
}
fn join(value: &JoinBinding, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let JoinBinding {
        id,
        signature,
        parameters,
        body,
    } = value;
    array!(out;
        n(id.0, out),
        n(signature.0, out),
        list(parameters, |id, out| n(id.0, out), out),
        n(*body as u64, out),
    )
}
fn alternative(value: &Alternative, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    let Alternative {
        pattern,
        binders,
        body,
    } = value;
    out.push(Header::Array(Some(3)))?;
    match pattern {
        AlternativePattern::Default => array!(out;n(0_u64, out)),
        AlternativePattern::Constructor(id) => array!(out;n(1_u64, out), n(id.0, out)),
        AlternativePattern::Literal(value) => array!(out;n(2_u64, out), scalar(value, out)),
    }?;
    list(binders, |id, out| n(id.0, out), out)?;
    n(*body as u64, out)
}
fn kind(value: &CaseKind, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        CaseKind::Algebraic(value) => array!(out;n(0_u64, out), type_identity_value(value, out)),
        CaseKind::Primitive(value) => array!(out;n(1_u64, out), rep(value, out)),
        CaseKind::MultiValue => array!(out;n(2_u64, out)),
        CaseKind::Polymorphic => array!(out;n(3_u64, out)),
    }
}
fn expression(value: &ExprFrame<usize>, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    match value {
        ExprFrame::Return(values) => array!(out;n(0_u64, out), list(values, atom, out)),
        ExprFrame::Enter { callee, signature } => {
            array!(out;n(1_u64, out), atom(callee, out), n(signature.0, out))
        }
        ExprFrame::Call {
            callee,
            signature,
            arguments,
        } => array!(out;
            n(2_u64, out),
            atom(callee, out),
            n(signature.0, out),
            list(arguments, atom, out),
        ),
        ExprFrame::Operation {
            operation,
            arguments,
        } => array!(out;n(3_u64, out), n(operation.0, out), list(arguments, atom, out)),
        ExprFrame::Construct {
            constructor,
            fields,
        } => array!(out;n(4_u64, out), n(constructor.0, out), list(fields, atom, out)),
        ExprFrame::Case {
            scrutinee,
            binder,
            scrutinee_results,
            kind: classification,
            alternatives,
        } => array!(out;
            n(5_u64, out),
            n(*scrutinee as u64, out),
            n(binder.0, out),
            result(scrutinee_results, out),
            kind(classification, out),
            list(alternatives, alternative, out),
        ),
        ExprFrame::Let { bindings, body } => {
            array!(out;n(6_u64, out), group(bindings, heap, out), n(*body as u64, out))
        }
        ExprFrame::LetJoins { bindings, body } => {
            array!(out;n(7_u64, out), group(bindings, join, out), n(*body as u64, out))
        }
        ExprFrame::Jump { join, arguments } => {
            array!(out;n(8_u64, out), n(join.0, out), list(arguments, atom, out))
        }
    }
}

fn type_graph(
    graph: &crate::type_graph::TypeGraph,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    use petgraph::visit::EdgeRef;
    out.push(Header::Array(Some(3)))?;
    n(2_u64, out)?;
    out.push(Header::Array(Some(graph.graph().node_count())))?;
    for node in graph.graph().node_weights() {
        type_node(node, out)?;
    }
    out.push(Header::Array(Some(graph.graph().edge_count())))?;
    for source in graph.graph().node_indices() {
        for edge in graph.ordered_edges(source) {
            array!(out; n(source.index() as u64, out), n(edge.target().index() as u64, out), type_role(*edge.weight(), out))?;
        }
    }
    Ok(())
}

fn type_node(value: &crate::type_graph::TypeNode, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    use crate::type_graph::{
        ForAllFlag, FunctionFlag, ParameterFlag, RootDomain, SourceBinderFlag, SyntaxRestriction,
        TypeLiteral, TypeNode,
    };
    match value {
        TypeNode::Root {
            domain,
            binders,
            rendered,
        } => array!(out;
            n(0_u64, out), n(match domain { RootDomain::Closed => 0_u64, RootDomain::ConstructorScheme => 1 }, out),
            list(binders, |flag, out| n(match flag { SourceBinderFlag::Specified => 1_u64, SourceBinderFlag::Inferred => 2 }, out), out), s(rendered, out)),
        TypeNode::Declaration {
            identity,
            parameters,
            form,
            restriction,
        } => array!(out;
            n(1_u64, out), type_identity_value(identity, out),
            list(parameters, |flag, out| n(match flag { ParameterFlag::NamedRequired => 0_u64, ParameterFlag::NamedSpecified => 1, ParameterFlag::NamedInferred => 2, ParameterFlag::AnonymousVisible => 3 }, out), out),
            type_form(form, out), n(match restriction { SyntaxRestriction::None => 0_u64, SyntaxRestriction::EffectHead => 1 }, out)),
        TypeNode::ConstructorTemplate {
            constructor,
            identity: _,
        } => array!(out; n(2_u64, out), n(constructor.0, out)),
        TypeNode::Bound(index) => array!(out; n(3_u64, out), n(*index, out)),
        TypeNode::NominalApplication => array!(out; n(4_u64, out)),
        TypeNode::Application => array!(out; n(5_u64, out)),
        TypeNode::Function(flag) => {
            array!(out; n(6_u64, out), n(match flag { FunctionFlag::TypeToType => 0_u64, FunctionFlag::TypeToConstraint => 1, FunctionFlag::ConstraintToType => 2, FunctionFlag::ConstraintToConstraint => 3 }, out))
        }
        TypeNode::ForAll(flag) => {
            array!(out; n(7_u64, out), n(match flag { ForAllFlag::Required => 0_u64, ForAllFlag::Specified => 1, ForAllFlag::Inferred => 2 }, out))
        }
        TypeNode::Literal(literal) => match literal {
            TypeLiteral::Natural(value) => array!(out; n(8_u64, out), n(0_u64, out), s(value, out)),
            TypeLiteral::Symbol(value) => array!(out; n(8_u64, out), n(1_u64, out), s(value, out)),
            TypeLiteral::Character(value) => {
                array!(out; n(8_u64, out), n(2_u64, out), n(*value as u32, out))
            }
        },
    }
}
fn type_form(
    value: &crate::type_graph::DeclarationForm,
    out: &mut Encoder<'_>,
) -> Result<(), ParseError> {
    use crate::type_graph::{DeclarationForm, NominalHeadKind};
    match value {
        DeclarationForm::Data => array!(out; n(0_u64, out)),
        DeclarationForm::Newtype { eta_arity } => array!(out; n(1_u64, out), n(*eta_arity, out)),
        DeclarationForm::Text => array!(out; n(2_u64, out)),
        DeclarationForm::Integer => array!(out; n(3_u64, out)),
        DeclarationForm::Natural => array!(out; n(4_u64, out)),
        DeclarationForm::Scalar(value) => array!(out; n(5_u64, out), rep(value, out)),
        DeclarationForm::Opaque { head_kind, reason } => {
            array!(out; n(6_u64, out), n(match head_kind { NominalHeadKind::Constructor => 0_u64, NominalHeadKind::Family => 1 }, out), s(reason, out))
        }
    }
}
fn type_role(value: crate::type_graph::TypeEdge, out: &mut Encoder<'_>) -> Result<(), ParseError> {
    use crate::type_graph::TypeEdge;
    match value {
        TypeEdge::BinderKind(ordinal) => array!(out; n(0_u64, out), n(ordinal, out)),
        TypeEdge::Body => array!(out; n(1_u64, out)),
        TypeEdge::Head => array!(out; n(2_u64, out)),
        TypeEdge::Argument(ordinal) => array!(out; n(3_u64, out), n(ordinal, out)),
        TypeEdge::Function => array!(out; n(4_u64, out)),
        TypeEdge::ApplyArgument => array!(out; n(5_u64, out)),
        TypeEdge::Multiplicity => array!(out; n(6_u64, out)),
        TypeEdge::Domain => array!(out; n(7_u64, out)),
        TypeEdge::Codomain => array!(out; n(8_u64, out)),
        TypeEdge::Kind => array!(out; n(9_u64, out)),
        TypeEdge::Constructor(tag) => array!(out; n(10_u64, out), n(tag, out)),
        TypeEdge::Field {
            ordinal,
            source_rep,
        } => array!(out; n(11_u64, out), n(ordinal, out), rep(&source_rep, out)),
        TypeEdge::AliasRhs => array!(out; n(12_u64, out)),
    }
}

#[cfg(test)]
mod legacy;
#[cfg(test)]
mod tests;
