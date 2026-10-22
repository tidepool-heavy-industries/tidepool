use super::*;
use crate::execution_schema::testing;
use crate::execution_schema::testing::{envelope, identity, projected_group, wire_program};
use crate::execution_schema::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn streamed_groups_match_frozen_codec_and_exact_budget(result: i64, ordinal: u32) {
        let mut wire = wire_program();
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
            bits: 64, bytes: result.to_be_bytes().to_vec(),
        })]);
        let group = projected_group(wire, ordinal).unwrap();
        let encoded = encode_projected_group(&group);
        prop_assert_eq!(&encoded, &legacy::encode_projected_group(&group));
        let operation = InventoryOperation::new(InventoryDecodeLimits { max_work: encoded.len(), ..Default::default() });
        prop_assert_eq!(hash_projected_group(&group, &operation).unwrap(), <[u8;32]>::from(Sha256::digest(&encoded)));
        prop_assert_eq!(operation.work_usage().unwrap(), (encoded.len(), 0));
        let short = InventoryOperation::new(InventoryDecodeLimits { max_work: encoded.len()-1, ..Default::default() });
        prop_assert!(matches!(hash_projected_group(&group, &short), Err(ParseError::LimitExceeded("work"))));
        prop_assert!(short.work_usage().unwrap().0 <= encoded.len()-1);
    }
}

#[test]
fn refused_write_does_not_consume_output_or_payload_budget() {
    let operation = InventoryOperation::new(InventoryDecodeLimits {
        max_work: 3,
        ..Default::default()
    });
    let mut output = Vec::new();
    operation
        .with_budget(|budget| {
            let mut sink = Sink {
                destination: Destination::Bytes(&mut output),
                budget: Some(budget),
            };
            sink.write_all(&[1, 2]).unwrap();
            assert!(matches!(
                sink.write_all(&[3, 4]),
                Err(ParseError::LimitExceeded("work"))
            ));
            Ok(())
        })
        .unwrap();
    assert_eq!(output, [1, 2]);
    assert_eq!(operation.work_usage().unwrap(), (2, 1));
}

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
        identity: crate::execution_schema::testing::constructor_identity(
            name.clone(),
            identity("Owner", "Record"),
        ),
        host_id: crate::DataConId(100),
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
    use crate::type_graph::{DeclarationForm, NominalHeadKind};
    wire.types = testing::closed_type_roots(&[
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
        CaseKind::Algebraic(testing::type_identity(name)),
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
    assert_eq!(encoded, legacy::encode_wire_program(&wire));
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
        encode_projected_group(&expected),
        legacy::encode_projected_group(&expected)
    );
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
        encode_module_products(&products),
        legacy::encode_module_products(&products)
    );
    assert_eq!(
        parse_module_products(
            &encode_module_products(&products),
            &requirements,
            InventoryDecodeLimits::default(),
        )
        .unwrap(),
        products,
    );
}
