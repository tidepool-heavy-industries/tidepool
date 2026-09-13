use tidepool_repr::execution_schema::parse_program;
use tidepool_repr::execution_schema::{
    DecodeLimits, ForeignConvention, GlobalDecl, GlobalId, OperationIdentity,
    ProgramRequirements, RuntimeRep, SignatureId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::execution_schema::testing::{envelope, identity, target, wire_program};

const M3_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor");
const SCHEMA6_SEED_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-execution-schema-encode/fixtures/schema6-intrinsic.cbor");

#[test]
fn w5_no_success_is_not_a_successful_empty_return() {
    use tidepool_repr::execution_schema::{ExprFrame, ResultContract};
    let mut wire = wire_program();
    wire.signatures[0].results = ResultContract::NoSuccess;
    wire.expressions.nodes[0] = ExprFrame::Return(vec![]);
    assert!(tidepool_repr::execution_schema::testing::prepare(wire).is_err());
    assert!(!ResultContract::Returns(vec![]).satisfies(&ResultContract::NoSuccess));
    assert!(ResultContract::NoSuccess.satisfies(&ResultContract::Returns(vec![])));
}

#[test]
fn w5_no_success_case_merge_preserves_successful_representations() {
    use tidepool_repr::execution_schema::ResultContract;
    let result = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
    assert_eq!(ResultContract::NoSuccess.merge_alternative(&result), Some(result.clone()));
    assert_eq!(result.merge_alternative(&ResultContract::NoSuccess), Some(result.clone()));
    assert_eq!(result.merge_alternative(&ResultContract::Returns(vec![])), None);
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
    assert!(!reverse.dead_end);
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
    assert_eq!(constructor.identity.record_parent.as_deref(), Some("FixtureRecord"));

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
    assert_eq!(operation.signature.0, 2);
    assert_eq!(program.signatures()[operation.signature.0 as usize].arguments,
        vec![RuntimeRep::Float(64)]);
    assert_eq!(program.signatures()[operation.signature.0 as usize].results,
        vec![RuntimeRep::Float(64)]);
}

#[test]
fn representative_recursive_import_contract_compiles() {
    let envelope = envelope();
    let mut program = wire_program();
    program.envelope = envelope.clone();
    let signature = program.signatures.first_mut().expect("fixture signature");
    signature.arguments = vec![RuntimeRep::LiftedRef];
    signature.results = vec![RuntimeRep::LiftedRef];
    program.globals.push(GlobalDecl {
        identity: identity("Fixture.Dependency", "imported"),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(SignatureId(0)),
        dead_end: false,
        required_evaluated: false,
        required_generation: Some(7),
    });
    let binding_group = program.bindings.first_mut().expect("fixture binding");
    if let tidepool_repr::execution_schema::Group::NonRecursive(binding) = binding_group {
        binding.binding.rhs = tidepool_repr::execution_schema::HeapRhs::Thunk {
            signature: SignatureId(0),
            update: tidepool_repr::execution_schema::UpdatePolicy::Memoize,
            captures: vec![tidepool_repr::execution_schema::ValueRef::Global(GlobalId(0))],
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
