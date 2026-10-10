//! Typed test codec. Encoding describes data; it never issues compiler evidence.
//!
//! Keep matches and struct destructuring exhaustive so adding a schema field or
//! variant requires updating this codec. Semantic admission remains in the reader.

use ciborium::value::Value;
use tidepool_repr::execution_schema::*;

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
    let definitions_value = ProgramDefinitions {
        envelope: content.envelope().clone(),
        signatures: content.signatures().to_vec(),
        globals: content.globals().to_vec(),
        constructors: content.constructors().to_vec(),
        operations: content.operations().to_vec(),
        expressions: content.expressions().clone(),
        bindings: content.bindings().to_vec(),
        types: content.types().clone(),
        sites: content.sites().to_vec(),
        constructor_replies: content.constructor_replies().to_vec(),
        json_layout: content.json_layout().copied(),
    };
    let ProgramDefinitions {
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
    } = &definitions_value;
    let mut fields = vec![
        s("TPGRP"),
        n(1_u64),
        n(group.original_ordinal()),
        list(group.binders(), symbol),
    ];
    fields.extend(definitions(
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
    types: &tidepool_repr::type_graph::TypeGraph,
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
                    ConstructorReply::StaticWithSite { reply, field, payload_field, capture_input } =>
                        a([n(2_u64), n(reply.0), n(*field), n(*payload_field),
                            capture_input.map_or(Value::Null, n)]),
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
        family,
        result_rep,
        field_reps,
        strict_fields,
        layout: storage,
        tag,
        family_size,
    } = value;
    a([
        symbol(identity),
        symbol(family),
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
        CaseKind::Algebraic(value) => a([n(0_u64), symbol(value)]),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::testing::{
        envelope, identity, projected_group, wire_program,
    };

    #[test]
    fn all_variant_fixture_cannot_bypass_semantic_admission() {
        let mut wire = wire_program();
        let reps = vec![
            RuntimeRep::Void,
            RuntimeRep::LiftedRef,
            RuntimeRep::UnliftedRef,
            RuntimeRep::Address,
            RuntimeRep::Int(8),
            RuntimeRep::Word(32),
            RuntimeRep::Float(64),
        ];
        let literals = vec![
            ScalarLiteral::Int {
                bits: 8,
                bytes: vec![7],
            },
            ScalarLiteral::Word {
                bits: 32,
                bytes: vec![0; 4],
            },
            ScalarLiteral::Float {
                bits: 64,
                bytes: vec![0; 8],
            },
            ScalarLiteral::Bytes(vec![0, 255]),
            ScalarLiteral::NullAddress,
        ];
        let mut name = identity("Owner", "field");
        name.record_parent = Some("Record".into());
        wire.envelope.target = TargetDescriptor {
            architecture: Architecture::Aarch64,
            endianness: Endianness::Big,
            pointer_width: 64,
            word_width: 64,
            abi: "test".into(),
            features: vec!["feature".into()],
        };
        wire.signatures = vec![
            Signature {
                arguments: reps.clone(),
                results: ResultContract::Returns(reps.clone()),
            },
            Signature {
                arguments: vec![],
                results: ResultContract::NoSuccess,
            },
            Signature {
                arguments: vec![],
                results: ResultContract::CallerResult,
            },
        ];
        wire.globals = vec![GlobalDecl {
            identity: name.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: true,
            required_generation: Some(19),
        }];
        wire.constructors.push(ConstructorDecl {
            identity: name.clone(),
            host_id: tidepool_repr::DataConId(100),
            family: identity("Owner", "Record"),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: reps.clone(),
            strict_fields: vec![true, false],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::LiftedRef,
                    offset: 8,
                }],
                alignment: 8,
                payload_size: 16,
                root_mask: vec![true],
            },
            tag: 1,
            family_size: 2,
        });
        use tidepool_repr::type_graph::{DeclarationForm, NominalHeadKind};
        wire.types = crate::prepared::closed_type_roots(&[
            (identity("Owner", "Text"), DeclarationForm::Text),
            (identity("Owner", "Integer"), DeclarationForm::Integer),
            (identity("Owner", "Natural"), DeclarationForm::Natural),
            (
                identity("Owner", "Int#"),
                DeclarationForm::Scalar(RuntimeRep::Int(64)),
            ),
            (
                identity("Owner", "Opaque"),
                DeclarationForm::Opaque {
                    head_kind: NominalHeadKind::Constructor,
                    reason: "test".into(),
                },
            ),
        ]);
        wire.sites = [
            SiteDelivery::HostAnswer,
            SiteDelivery::LiveReentry,
            SiteDelivery::ExitCellFill,
            SiteDelivery::TerminalCapture,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, delivery)| SiteRow {
            site: i as u64,
            origin: "test".into(),
            ordinal: i as u64,
            delivery,
            wire: TypeNodeId(1),
            inputs: vec![TypeNodeId(2)],
        })
        .collect();
        wire.constructor_replies = vec![
            (ConstructorId(0), ConstructorReply::Static(TypeNodeId(1))),
            (ConstructorId(1), ConstructorReply::AtSite),
        ];
        wire.json_layout = Some(JsonLayout {
            object: ConstructorId(0),
            array: ConstructorId(1),
            string: ConstructorId(2),
            number: ConstructorId(3),
            bool_: ConstructorId(4),
            null: ConstructorId(5),
            map_bin: ConstructorId(6),
            map_tip: ConstructorId(7),
            true_: ConstructorId(8),
            false_: ConstructorId(9),
            cons: ConstructorId(10),
            nil: ConstructorId(11),
            scientific: ConstructorId(12),
            integer_small: ConstructorId(13),
            integer_positive: ConstructorId(14),
            integer_negative: ConstructorId(15),
            text: ConstructorId(16),
            int: ConstructorId(17),
        });
        let operations = vec![
            OperationIdentity::PrimOp("op".into()),
            OperationIdentity::Intrinsic {
                symbol: "intrinsic".into(),
                convention: ForeignConvention::CCall,
            },
            OperationIdentity::Capability {
                name: "capability".into(),
            },
            OperationIdentity::JsonDecode {
                left: ConstructorId(0),
                right: ConstructorId(1),
            },
            OperationIdentity::JsonEncode,
        ];
        wire.operations = operations
            .into_iter()
            .chain(
                [
                    WiredInErrorKind::PatternMatch,
                    WiredInErrorKind::NonExhaustiveGuards,
                    WiredInErrorKind::RecordSelector,
                    WiredInErrorKind::RecordConstruction,
                    WiredInErrorKind::NoMethodBinding,
                    WiredInErrorKind::DeferredType,
                    WiredInErrorKind::Impossible,
                    WiredInErrorKind::ImpossibleConstraint,
                    WiredInErrorKind::Absent,
                    WiredInErrorKind::AbsentConstraint,
                    WiredInErrorKind::AbsentSumField,
                ]
                .map(|kind| OperationIdentity::WiredInError { kind }),
            )
            .map(|identity| OperationDecl {
                identity,
                signature: SignatureId(0),
            })
            .collect();
        let mut atoms = literals
            .iter()
            .cloned()
            .map(Atom::Scalar)
            .collect::<Vec<_>>();
        atoms.extend([
            Atom::Void,
            Atom::Rubbish(RuntimeRep::LiftedRef),
            Atom::Ref(ValueRef::Local(ValueId(1))),
            Atom::Ref(ValueRef::Global(GlobalId(0))),
        ]);
        let heap_values = vec![
            HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Bytes(vec![0, 255]),
            },
            HeapBinding {
                id: ValueId(3),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: atoms.clone(),
                },
            },
            HeapBinding {
                id: ValueId(4),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Local(ValueId(1))],
                    body: 0,
                },
            },
            HeapBinding {
                id: ValueId(5),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::SingleEntry,
                    captures: vec![ValueRef::Global(GlobalId(0))],
                    body: 0,
                },
            },
        ];
        wire.bindings.push(Group::Recursive(
            heap_values
                .iter()
                .map(|binding| TopBinding {
                    identity: name.clone(),
                    binding: binding.clone(),
                })
                .collect(),
        ));
        wire.expressions.nodes = vec![
            ExprFrame::Return(atoms.clone()),
            ExprFrame::Enter {
                callee: atoms[0].clone(),
                signature: SignatureId(0),
            },
            ExprFrame::Call {
                callee: atoms[0].clone(),
                signature: SignatureId(0),
                arguments: atoms.clone(),
            },
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: atoms.clone(),
            },
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: atoms.clone(),
            },
            ExprFrame::Let {
                bindings: Group::Recursive(heap_values.clone()),
                body: 0,
            },
            ExprFrame::Let {
                bindings: Group::NonRecursive(heap_values[0].clone()),
                body: 0,
            },
            ExprFrame::LetJoins {
                bindings: Group::NonRecursive(JoinBinding {
                    id: JoinId(0),
                    signature: SignatureId(0),
                    parameters: vec![ValueId(1)],
                    body: 0,
                }),
                body: 0,
            },
            ExprFrame::LetJoins {
                bindings: Group::Recursive(vec![]),
                body: 0,
            },
            ExprFrame::Jump {
                join: JoinId(0),
                arguments: atoms,
            },
        ];
        for kind in [
            CaseKind::Algebraic(name),
            CaseKind::Primitive(RuntimeRep::Int(8)),
            CaseKind::MultiValue,
            CaseKind::Polymorphic,
        ] {
            wire.expressions.nodes.push(ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(6),
                scrutinee_results: ResultContract::Returns(reps.clone()),
                kind,
                alternatives: [
                    AlternativePattern::Default,
                    AlternativePattern::Constructor(ConstructorId(0)),
                    AlternativePattern::Literal(literals[0].clone()),
                ]
                .into_iter()
                .map(|pattern| Alternative {
                    pattern,
                    binders: vec![ValueId(7)],
                    body: 0,
                })
                .collect(),
            });
        }
        let encoded = encode_wire_program(&wire);
        let requirements = ProgramRequirements {
            schema_version: SCHEMA_VERSION,
            projection_profile: envelope().projection_profile,
            toolchain: envelope().toolchain,
            execution_abi_version: EXECUTION_ABI_VERSION,
            target: wire.envelope.target.clone(),
        };
        // Decode must traverse the complete grammar and reach the same semantic
        // refusal as the typed owner. A malformed encoding cannot satisfy this.
        let expected = testing::prepare(wire).unwrap_err();
        assert_eq!(
            parse_program(&encoded, &requirements, DecodeLimits::default()).unwrap_err(),
            expected
        );
    }

    #[test]
    fn typed_program_and_entry_free_group_cross_the_normal_readers() {
        let wire = wire_program();
        let requirements = ProgramRequirements {
            schema_version: SCHEMA_VERSION,
            projection_profile: envelope().projection_profile,
            toolchain: envelope().toolchain,
            execution_abi_version: EXECUTION_ABI_VERSION,
            target: envelope().target,
        };
        let expected = testing::prepare(wire.clone()).unwrap();
        assert_eq!(
            parse_program(
                &encode_wire_program(&wire),
                &requirements,
                DecodeLimits::default()
            )
            .unwrap(),
            expected
        );
        let expected = projected_group(wire, 17).unwrap();
        assert_eq!(
            parse_projected_group(
                &encode_projected_group(&expected),
                &requirements,
                DecodeLimits::default()
            )
            .unwrap(),
            expected
        );
        let products = vec![RawModuleProduct {
            unit: "fixture".into(),
            module: "Fixture".into(),
            interface: vec![0x42],
            groups: vec![expected],
        }];
        assert_eq!(
            parse_module_products(
                &encode_module_products(&products),
                &requirements,
                DecodeLimits::default(),
            )
            .unwrap(),
            products,
        );
    }
}
