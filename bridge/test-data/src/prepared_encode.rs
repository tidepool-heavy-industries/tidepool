//! Representation-only encoding fixtures use the production exhaustive codec.

pub use tidepool_repr::execution_schema::encoding::{
    encode_module_products, encode_projected_group, encode_wire_program,
};

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::testing::{
        envelope, identity, projected_group, wire_program,
    };
    use tidepool_repr::execution_schema::*;

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
            identity: tidepool_repr::execution_schema::testing::constructor_identity(
                name.clone(),
                identity("Owner", "Record"),
            ),
            host_id: tidepool_repr::DataConId(100),
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
                InventoryDecodeLimits::default(),
            )
            .unwrap(),
            products,
        );
    }
}
