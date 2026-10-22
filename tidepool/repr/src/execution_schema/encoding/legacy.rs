//! Frozen encoder from before the streaming implementation, used only as a
//! differential byte oracle. Keep it immutable; never update it in lockstep
//! with the production codec. Version or replace the oracle deliberately when
//! the wire format changes, retaining independently verified expected bytes.

use super::*;
use ciborium::value::Value;

fn n(value: impl Into<u64>) -> Value {
    Value::Integer(value.into().into())
}
fn s(value: &str) -> Value {
    Value::Text(value.into())
}
fn a(values: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(values.into_iter().collect())
}
fn list<T>(values: &[T], encode: impl Fn(&T) -> Value) -> Value {
    a(values.iter().map(encode))
}
fn option<T>(value: &Option<T>, encode: impl Fn(&T) -> Value) -> Value {
    match value {
        None => a([n(0_u64)]),
        Some(value) => a([n(1_u64), encode(value)]),
    }
}
fn bytes(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    ciborium::ser::into_writer(value, &mut output).expect("in-memory CBOR encoding");
    output
}

/// Encode current typed program data without validating or issuing authority.
/// Callers may deliberately supply invalid values when testing reader refusal.
pub fn encode_wire_program(wire: &WireProgram) -> Vec<u8> {
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
    let mut fields = definitions(
        envelope,
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
    );
    fields.insert(11, n(entry.0));
    fields.insert(0, s("TPSTG"));
    bytes(&a(fields))
}

/// Encode current typed module products without issuing compiler evidence.
/// Group payloads use the same current prepared codec as standalone fixtures.
pub fn encode_module_products(products: &[RawModuleProduct]) -> Vec<u8> {
    bytes(&a([
        s("TPMOD"),
        n(MODULE_PRODUCTS_VERSION),
        list(products, |product| {
            let RawModuleProduct {
                unit,
                module,
                interface,
                groups,
            } = product;
            a([
                s(unit),
                s(module),
                Value::Bytes(interface.clone()),
                list(groups, |group| Value::Bytes(encode_projected_group(group))),
            ])
        }),
    ]))
}

/// Encode an entry-free group; its existing typed validation is independent of
/// compiler provenance, which only the production toolchain can supply.
pub fn encode_projected_group(group: &ProjectedGroup) -> Vec<u8> {
    let content = group.definitions();
    let mut fields = vec![
        s("TPGRP"),
        n(2_u64),
        n(group.original_ordinal()),
        list(group.binders(), symbol),
    ];
    fields.extend(definitions(
        content.envelope(),
        content.signatures(),
        content.globals(),
        content.constructors(),
        content.operations(),
        content.expressions(),
        content.bindings(),
        content.types(),
        content.sites(),
        content.constructor_replies(),
        &content.json_layout().copied(),
    ));
    bytes(&a(fields))
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
    types: &crate::type_graph::TypeGraph,
    sites: &[SiteRow],
    constructor_replies: &[(ConstructorId, ConstructorReply)],
    json_layout: &Option<JsonLayout>,
) -> Vec<Value> {
    let ProgramEnvelope {
        schema_version,
        projection_profile,
        toolchain,
        execution_abi_version,
        target,
    } = envelope;
    let Expr { nodes } = expressions;
    vec![
        n(*schema_version),
        s(projection_profile),
        s(toolchain),
        n(*execution_abi_version),
        target_value(target),
        list(signatures, signature),
        list(globals, global),
        list(constructors, constructor),
        list(operations, operation),
        list(nodes, expression),
        list(bindings, |value| group(value, top)),
        encode_type_graph_value(types),
        list(sites, site),
        list(constructor_replies, |(id, reply)| {
            a([
                n(id.0),
                match reply {
                    ConstructorReply::Static(node) => a([n(0_u64), n(node.0)]),
                    ConstructorReply::AtSite => a([n(1_u64)]),
                    ConstructorReply::StaticWithSite {
                        reply,
                        field,
                        payload_field,
                        capture_input,
                    } => a([
                        n(2_u64),
                        n(reply.0),
                        n(*field),
                        n(*payload_field),
                        capture_input.map_or(Value::Null, n),
                    ]),
                },
            ])
        }),
        option(json_layout, json),
    ]
}
fn target_value(value: &TargetDescriptor) -> Value {
    let TargetDescriptor {
        architecture,
        endianness,
        pointer_width,
        word_width,
        abi,
        features,
    } = value;
    a([
        n(match architecture {
            Architecture::X86_64 => 0_u64,
            Architecture::Aarch64 => 1,
        }),
        n(match endianness {
            Endianness::Little => 0_u64,
            Endianness::Big => 1,
        }),
        n(*pointer_width),
        n(*word_width),
        s(abi),
        list(features, |value| s(value)),
    ])
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
        s(unit),
        s(module),
        s(namespace),
        s(occurrence),
        option(record_parent, |value| s(value)),
    ])
}
fn type_identity_value(value: &TypeIdentity) -> Value {
    a([
        symbol(&value.symbol),
        match &value.version {
            TypeVersion::WiredIn => a([n(0_u64)]),
            TypeVersion::GhcAbi(bytes) => a([n(1_u64), Value::Bytes(bytes.to_vec())]),
        },
    ])
}
fn constructor_identity_value(value: &ConstructorIdentity) -> Value {
    a([symbol(&value.symbol), type_identity_value(&value.family)])
}

fn rep(value: &RuntimeRep) -> Value {
    match value {
        RuntimeRep::Void => a([n(0_u64)]),
        RuntimeRep::LiftedRef => a([n(1_u64)]),
        RuntimeRep::UnliftedRef => a([n(2_u64)]),
        RuntimeRep::Address => a([n(3_u64)]),
        RuntimeRep::Int(bits) => a([n(4_u64), n(*bits)]),
        RuntimeRep::Word(bits) => a([n(5_u64), n(*bits)]),
        RuntimeRep::Float(bits) => a([n(6_u64), n(*bits)]),
    }
}
fn result(value: &ResultContract) -> Value {
    match value {
        ResultContract::Returns(reps) => a([n(0_u64), list(reps, rep)]),
        ResultContract::NoSuccess => a([n(1_u64)]),
        ResultContract::CallerResult => a([n(2_u64)]),
    }
}
fn signature(value: &Signature) -> Value {
    let Signature { arguments, results } = value;
    a([list(arguments, rep), result(results)])
}
fn layout(value: &CheckedLayout) -> Value {
    let CheckedLayout {
        fields,
        alignment,
        payload_size,
        root_mask,
    } = value;
    a([
        list(fields, |value| {
            let FieldLayout {
                rep: representation,
                offset,
            } = value;
            a([rep(representation), n(*offset)])
        }),
        n(*alignment),
        n(*payload_size),
        list(root_mask, |value| Value::Bool(*value)),
    ])
}
fn constructor(value: &ConstructorDecl) -> Value {
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
    a([
        constructor_identity_value(identity),
        list(field_reps, rep),
        list(strict_fields, |value| Value::Bool(*value)),
        layout(storage),
        rep(result_rep),
        n(*tag),
        n(*family_size),
        n(host_id.0),
    ])
}
fn site(value: &SiteRow) -> Value {
    let SiteRow {
        site,
        origin,
        ordinal,
        delivery,
        wire,
        inputs,
    } = value;
    a([
        n(*site),
        s(origin),
        n(*ordinal),
        n(match delivery {
            SiteDelivery::HostAnswer => 0_u64,
            SiteDelivery::LiveReentry => 1,
            SiteDelivery::ExitCellFill => 2,
            SiteDelivery::TerminalCapture => 3,
        }),
        n(wire.0),
        list(inputs, |id| n(id.0)),
    ])
}
fn global(value: &GlobalDecl) -> Value {
    let GlobalDecl {
        identity,
        rep: representation,
        entry_signature,
        required_evaluated,
        required_generation,
    } = value;
    a([
        symbol(identity),
        rep(representation),
        option(entry_signature, |id| n(id.0)),
        Value::Bool(*required_evaluated),
        option(required_generation, |value| n(*value)),
    ])
}
fn operation(value: &OperationDecl) -> Value {
    let OperationDecl {
        identity,
        signature,
    } = value;
    let identity = match identity {
        OperationIdentity::PrimOp(name) => a([n(0_u64), s(name)]),
        OperationIdentity::Intrinsic { symbol, convention } => a([
            n(1_u64),
            s(symbol),
            a([n(match convention {
                ForeignConvention::CCall => 0_u64,
            })]),
        ]),
        OperationIdentity::Capability { name } => a([n(2_u64), s(name)]),
        OperationIdentity::WiredInError { kind } => a([
            n(3_u64),
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
            }),
        ]),
        OperationIdentity::JsonDecode { left, right } => a([n(4_u64), n(left.0), n(right.0)]),
        OperationIdentity::JsonEncode => a([n(5_u64)]),
    };
    a([identity, n(signature.0)])
}
fn json(value: &JsonLayout) -> Value {
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
    a([
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
    ]
    .map(|id| n(id.0)))
}
fn reference(value: &ValueRef) -> Value {
    match value {
        ValueRef::Local(id) => a([n(0_u64), n(id.0)]),
        ValueRef::Global(id) => a([n(1_u64), n(id.0)]),
    }
}
fn scalar(value: &ScalarLiteral) -> Value {
    match value {
        ScalarLiteral::Int { bits, bytes } => a([n(0_u64), n(*bits), Value::Bytes(bytes.clone())]),
        ScalarLiteral::Word { bits, bytes } => a([n(1_u64), n(*bits), Value::Bytes(bytes.clone())]),
        ScalarLiteral::Float { bits, bytes } => {
            a([n(2_u64), n(*bits), Value::Bytes(bytes.clone())])
        }
        ScalarLiteral::Bytes(bytes) => a([n(4_u64), Value::Bytes(bytes.clone())]),
        ScalarLiteral::NullAddress => a([n(5_u64)]),
    }
}
fn atom(value: &Atom) -> Value {
    match value {
        Atom::Ref(value) => a([n(0_u64), reference(value)]),
        Atom::Scalar(value) => a([n(1_u64), scalar(value)]),
        Atom::Void => a([n(2_u64)]),
        Atom::Rubbish(value) => a([n(3_u64), rep(value)]),
    }
}
fn group<T>(value: &Group<T>, encode: impl Fn(&T) -> Value) -> Value {
    match value {
        Group::NonRecursive(value) => a([n(0_u64), encode(value)]),
        Group::Recursive(values) => a([n(1_u64), list(values, encode)]),
    }
}
fn heap(value: &HeapBinding) -> Value {
    let HeapBinding { id, rhs } = value;
    let rhs = match rhs {
        HeapRhs::Function {
            signature,
            parameters,
            captures,
            body,
        } => a([
            n(0_u64),
            n(signature.0),
            list(parameters, |id| n(id.0)),
            list(captures, reference),
            n(*body as u64),
        ]),
        HeapRhs::Thunk {
            signature,
            update,
            captures,
            body,
        } => a([
            n(1_u64),
            n(signature.0),
            n(match update {
                UpdatePolicy::Memoize => 0_u64,
                UpdatePolicy::SingleEntry => 1,
            }),
            list(captures, reference),
            n(*body as u64),
        ]),
        HeapRhs::Constructor {
            constructor,
            fields,
        } => a([n(2_u64), n(constructor.0), list(fields, atom)]),
        HeapRhs::Bytes(bytes) => a([n(3_u64), Value::Bytes(bytes.clone())]),
    };
    a([n(id.0), rhs])
}
fn top(value: &TopBinding) -> Value {
    let TopBinding { identity, binding } = value;
    a([symbol(identity), heap(binding)])
}
fn join(value: &JoinBinding) -> Value {
    let JoinBinding {
        id,
        signature,
        parameters,
        body,
    } = value;
    a([
        n(id.0),
        n(signature.0),
        list(parameters, |id| n(id.0)),
        n(*body as u64),
    ])
}
fn alternative(value: &Alternative) -> Value {
    let Alternative {
        pattern,
        binders,
        body,
    } = value;
    let pattern = match pattern {
        AlternativePattern::Default => a([n(0_u64)]),
        AlternativePattern::Constructor(id) => a([n(1_u64), n(id.0)]),
        AlternativePattern::Literal(value) => a([n(2_u64), scalar(value)]),
    };
    a([pattern, list(binders, |id| n(id.0)), n(*body as u64)])
}
fn kind(value: &CaseKind) -> Value {
    match value {
        CaseKind::Algebraic(value) => a([n(0_u64), type_identity_value(value)]),
        CaseKind::Primitive(value) => a([n(1_u64), rep(value)]),
        CaseKind::MultiValue => a([n(2_u64)]),
        CaseKind::Polymorphic => a([n(3_u64)]),
    }
}
fn expression(value: &ExprFrame<usize>) -> Value {
    match value {
        ExprFrame::Return(values) => a([n(0_u64), list(values, atom)]),
        ExprFrame::Enter { callee, signature } => a([n(1_u64), atom(callee), n(signature.0)]),
        ExprFrame::Call {
            callee,
            signature,
            arguments,
        } => a([
            n(2_u64),
            atom(callee),
            n(signature.0),
            list(arguments, atom),
        ]),
        ExprFrame::Operation {
            operation,
            arguments,
        } => a([n(3_u64), n(operation.0), list(arguments, atom)]),
        ExprFrame::Construct {
            constructor,
            fields,
        } => a([n(4_u64), n(constructor.0), list(fields, atom)]),
        ExprFrame::Case {
            scrutinee,
            binder,
            scrutinee_results,
            kind: classification,
            alternatives,
        } => a([
            n(5_u64),
            n(*scrutinee as u64),
            n(binder.0),
            result(scrutinee_results),
            kind(classification),
            list(alternatives, alternative),
        ]),
        ExprFrame::Let { bindings, body } => a([n(6_u64), group(bindings, heap), n(*body as u64)]),
        ExprFrame::LetJoins { bindings, body } => {
            a([n(7_u64), group(bindings, join), n(*body as u64)])
        }
        ExprFrame::Jump { join, arguments } => a([n(8_u64), n(join.0), list(arguments, atom)]),
    }
}
