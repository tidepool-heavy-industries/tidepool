use tidepool_repr::execution_schema::parse_program;
use tidepool_repr::execution_schema::testing::{envelope, identity, target, wire_program};
use tidepool_repr::execution_schema::{
    DecodeLimits, ForeignConvention, GlobalDecl, GlobalId, OperationDecl, OperationIdentity,
    ProgramRequirements, ResultContract, RuntimeRep, Signature, SignatureId, WiredInErrorKind,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

const M3_ARTIFACT: &[u8] =
    include_bytes!("../../../bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor");
const SCHEMA6_SEED_ARTIFACT: &[u8] = include_bytes!(
    "../../../bridge/haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor"
);

#[test]
fn w5_no_success_is_not_a_successful_empty_return() {
    use tidepool_repr::execution_schema::{ExprFrame, ResultContract};
    let mut wire = wire_program();
    wire.signatures[0].results = ResultContract::NoSuccess;
    wire.expressions.nodes[0] = ExprFrame::Return(vec![]);
    assert!(tidepool_repr::execution_schema::testing::prepare(wire).is_err());
    assert!(!ResultContract::Returns(vec![]).satisfies(&ResultContract::NoSuccess));
    assert!(ResultContract::NoSuccess.satisfies(&ResultContract::Returns(vec![])));
    assert_eq!(
        ResultContract::Returns(vec![]).returned_reps(),
        Some(&[][..])
    );
    assert_eq!(ResultContract::NoSuccess.returned_reps(), None);
}

#[test]
fn w5_no_success_case_merge_preserves_successful_representations() {
    use tidepool_repr::execution_schema::ResultContract;
    let result = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
    assert_eq!(
        ResultContract::NoSuccess.merge_alternative(&result),
        Some(result.clone())
    );
    assert_eq!(
        result.merge_alternative(&ResultContract::NoSuccess),
        Some(result.clone())
    );
    assert_eq!(
        result.merge_alternative(&ResultContract::Returns(vec![])),
        None
    );
}

#[test]
fn capability_identity_requires_a_nonempty_name_and_successful_return() {
    let mut valid = wire_program();
    valid.operations.push(OperationDecl {
        identity: OperationIdentity::Capability {
            name: "ffi.lookup".into(),
        },
        signature: SignatureId(0),
    });
    tidepool_repr::execution_schema::testing::prepare(valid).unwrap();

    let mut empty_name = wire_program();
    empty_name.operations.push(OperationDecl {
        identity: OperationIdentity::Capability {
            name: String::new(),
        },
        signature: SignatureId(0),
    });
    assert!(matches!(
        tidepool_repr::execution_schema::testing::prepare(empty_name),
        Err(tidepool_repr::execution_schema::ParseError::Malformed(detail))
            if detail.contains("empty identity text")
    ));

    let mut no_success = wire_program();
    no_success.signatures.push(Signature {
        arguments: vec![],
        results: ResultContract::NoSuccess,
    });
    no_success.operations.push(OperationDecl {
        identity: OperationIdentity::Capability {
            name: "ffi.lookup".into(),
        },
        signature: SignatureId(1),
    });
    assert!(matches!(
        tidepool_repr::execution_schema::testing::prepare(no_success),
        Err(tidepool_repr::execution_schema::ParseError::InvalidSignature(_))
    ));
}

#[test]
fn wired_in_error_identity_requires_its_exact_nonreturning_signature() {
    const KINDS: [WiredInErrorKind; 11] = [
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
    ];

    for kind in KINDS {
        let expected_arguments = if kind == WiredInErrorKind::AbsentSumField {
            vec![]
        } else {
            vec![RuntimeRep::Address]
        };
        let mut valid = wire_program();
        valid.signatures.push(Signature {
            arguments: expected_arguments.clone(),
            results: ResultContract::NoSuccess,
        });
        valid.operations.push(OperationDecl {
            identity: OperationIdentity::WiredInError { kind },
            signature: SignatureId(1),
        });
        tidepool_repr::execution_schema::testing::prepare(valid).unwrap();

        for signature in [
            Signature {
                arguments: expected_arguments.clone(),
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: if expected_arguments.is_empty() {
                    vec![RuntimeRep::Address]
                } else {
                    vec![]
                },
                results: ResultContract::NoSuccess,
            },
        ] {
            let mut invalid = wire_program();
            invalid.signatures.push(signature);
            invalid.operations.push(OperationDecl {
                identity: OperationIdentity::WiredInError { kind },
                signature: SignatureId(1),
            });
            assert!(matches!(
                tidepool_repr::execution_schema::testing::prepare(invalid),
                Err(tidepool_repr::execution_schema::ParseError::InvalidSignature(_))
            ));
        }
    }
}

#[test]
fn haskell_m3_fixture_decodes_global_contract() {
    let requirements = ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: target(),
    };
    let program = parse_program(M3_ARTIFACT, &requirements, DecodeLimits::default())
        .expect("Haskell M3 fixture should decode under the Rust schema");
    let reverse = program
        .globals()
        .iter()
        .find(|global| global.identity.occurrence == "reverse")
        .expect("M3 fixture should retain Data.List.reverse as an imported global");
    assert!(reverse.required_evaluated);
    assert_eq!(reverse.required_generation, None);
    assert_eq!(reverse.identity.record_parent, None);
}

#[test]
fn haskell_schema6_seed_decodes_record_parent_and_intrinsic() {
    let requirements = ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: target(),
    };
    let program = parse_program(
        SCHEMA6_SEED_ARTIFACT,
        &requirements,
        DecodeLimits::default(),
    )
    .expect("Haskell schema-6 seed should decode under the Rust schema");
    let constructor = program
        .constructors()
        .iter()
        .find(|constructor| constructor.identity.occurrence == "RecordField")
        .expect("schema-6 seed should retain its record-field identity");
    assert_eq!(
        constructor.identity.record_parent.as_deref(),
        Some("FixtureRecord")
    );

    let operation = program
        .operations()
        .iter()
        .find(|operation| {
            operation.identity
                == (OperationIdentity::Intrinsic {
                    symbol: "rintDouble".into(),
                    convention: ForeignConvention::CCall,
                })
        })
        .expect("schema-6 seed should retain its intrinsic operation identity");
    assert_eq!(
        program.signatures()[operation.signature.0 as usize].arguments,
        vec![RuntimeRep::Float(64)]
    );
    assert_eq!(
        program.signatures()[operation.signature.0 as usize].results,
        ResultContract::Returns(vec![RuntimeRep::Float(64)])
    );

    let raise = program
        .operations()
        .iter()
        .find(|operation| operation.identity == OperationIdentity::PrimOp("raise#".into()))
        .expect("schema seed should retain its nonreturning raise operation");
    assert_eq!(
        program.signatures()[raise.signature.0 as usize].results,
        ResultContract::NoSuccess
    );
}

#[test]
fn representative_recursive_import_contract_compiles() {
    let envelope = envelope();
    let mut program = wire_program();
    program.envelope = envelope.clone();
    let signature = program.signatures.first_mut().expect("fixture signature");
    signature.arguments = vec![RuntimeRep::LiftedRef];
    signature.results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    program.globals.push(GlobalDecl {
        identity: identity("Fixture.Dependency", "imported"),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(SignatureId(0)),
        required_evaluated: false,
        required_generation: Some(7),
    });
    let binding_group = program.bindings.first_mut().expect("fixture binding");
    if let tidepool_repr::execution_schema::Group::NonRecursive(binding) = binding_group {
        binding.binding.rhs = tidepool_repr::execution_schema::HeapRhs::Thunk {
            signature: SignatureId(0),
            update: tidepool_repr::execution_schema::UpdatePolicy::Memoize,
            captures: vec![tidepool_repr::execution_schema::ValueRef::Global(GlobalId(
                0,
            ))],
            body: 0,
        };
    }

    let requirements = ProgramRequirements {
        schema_version: envelope.schema_version,
        projection_profile: envelope.projection_profile.clone(),
        toolchain: envelope.toolchain.clone(),
        execution_abi_version: envelope.execution_abi_version,
        target: target(),
    };

    assert_eq!(program.entry.0, 0);
    assert_eq!(requirements.target, program.envelope.target);
    assert_eq!(
        program.globals.first().expect("fixture global").identity,
        identity("Fixture.Dependency", "imported")
    );
}
