use tidepool_repr::execution_schema::{
    Architecture, Atom, CheckedLayout, ConstructorDecl, ConstructorId, Endianness, Expr,
    FieldLayout, GlobalDecl, GlobalId, Group, HeapBinding, HeapRhs, OperationDecl, ProgramEnvelope,
    ProgramRequirements, RuntimeRep, ScalarLiteral, Signature, SignatureId, SymbolIdentity,
    TargetDescriptor, TopBinding, UpdatePolicy, ValueId, ValueRef, WireProgram,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

fn symbol(module: &str, occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: "m3-fixture".into(),
        module: module.into(),
        namespace: "value".into(),
        occurrence: occurrence.into(),
    }
}

fn target() -> TargetDescriptor {
    TargetDescriptor {
        architecture: Architecture::X86_64,
        endianness: Endianness::Little,
        pointer_width: 64,
        word_width: 64,
        abi: "sysv64".into(),
        features: vec![],
    }
}

#[test]
fn representative_recursive_import_contract_compiles() {
    let envelope = ProgramEnvelope {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: target(),
    };
    let program = WireProgram {
        envelope: envelope.clone(),
        signatures: vec![Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        }],
        globals: vec![GlobalDecl {
            identity: symbol("Fixture.Dependency", "imported"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: false,
            required_generation: Some(7),
        }],
        constructors: vec![ConstructorDecl {
            tag: 1,
            family_size: 1,
            result_rep: RuntimeRep::LiftedRef,
            identity: symbol("Fixture.Vertical", "Box"),
            family: symbol("Fixture.Vertical", "Box"),
            field_reps: vec![RuntimeRep::Int(64)],
            strict_fields: vec![true],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::Int(64),
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![false],
            },
        }],
        operations: vec![OperationDecl {
            identity: "sub-int64".into(),
            signature: SignatureId(0),
        }],
        bindings: vec![Group::Recursive(vec![TopBinding {
            identity: symbol("Fixture.Vertical", "entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Global(GlobalId(0))],
                    body: Box::new(Expr::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: 42_i64.to_be_bytes().to_vec(),
                    })])),
                },
            },
        }])],
        entry: ValueId(0),
    };
    let requirements = ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: envelope.projection_profile.clone(),
        toolchain: envelope.toolchain.clone(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: target(),
    };

    assert_eq!(program.entry, ValueId(0));
    assert_eq!(requirements.target, program.envelope.target);
    assert_eq!(ConstructorId(0).0, 0);
}
