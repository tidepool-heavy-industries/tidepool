use tidepool_codegen::prepared_calls::FlatPap;
use tidepool_codegen::prepared_native::{PreparedNativeError, PreparedNativeProgram};
use tidepool_codegen::prepared_thunks::PreparedThunk;
use tidepool_repr::execution_schema::{
    link_program, parse_program, Architecture, DecodeLimits, Endianness, ImportedValue,
    LinkedProgram, MachineImports, ProgramRequirements, TargetDescriptor, UpdatePolicy, ValueId,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

fn requirements() -> ProgramRequirements {
    ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: vec![],
        },
    }
}

fn linked_fixture() -> LinkedProgram {
    let prepared = parse_program(
        include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor"),
        &requirements(),
        DecodeLimits::default(),
    )
    .unwrap();
    let values = prepared
        .globals()
        .iter()
        .map(|global| {
            let value = ImportedValue {
                identity: global.identity.clone(),
                rep: global.rep.clone(),
                entry_signature: global
                    .entry_signature
                    .map(|id| prepared.signatures()[id.0 as usize].clone()),
                evaluated: global.required_evaluated,
                generation: global.required_generation.unwrap_or(0),
            };
            (value.identity.clone(), value)
        })
        .collect();
    link_program(prepared, &MachineImports { values }).unwrap()
}

#[test]
fn worker_bytes_link_and_execute_directly_in_cranelift() {
    let linked = linked_fixture();
    let native = PreparedNativeProgram::compile(&linked).unwrap();
    let result = native.execute().unwrap();
    assert_eq!(result.constructor.0, 0);
    assert_eq!(result.fields, vec![42]);
}

#[test]
fn linked_function_runs_through_tail_adapter_pap_and_update_control() {
    let linked = linked_fixture();
    // The real worker fixture's `Box` binding is a checked function whose body
    // constructs Box from its Int64 parameter.
    let native = PreparedNativeProgram::compile_binding(&linked, ValueId(17)).unwrap();
    assert!(native.stack_map_count() > 0);
    let direct = native.execute_with_arguments(&[42]).unwrap();
    assert_eq!(direct.constructor.0, 1);
    assert_eq!(direct.fields, vec![42]);

    let signature = linked.prepared().signatures()[6].clone();
    let pap = FlatPap::new(ValueId(17), signature);
    assert_eq!(native.execute_flat_pap(&pap, &[42]).unwrap(), direct);

    let mut memo = PreparedThunk::new(UpdatePolicy::Memoize);
    assert_eq!(native.execute_memoized(&mut memo, &[42]).unwrap(), direct);
    assert_eq!(native.execute_memoized(&mut memo, &[99]).unwrap(), direct);

    let mut single = PreparedThunk::new(UpdatePolicy::SingleEntry);
    assert_eq!(native.execute_memoized(&mut single, &[42]).unwrap(), direct);
    assert!(matches!(
        native.execute_memoized(&mut single, &[42]),
        Err(PreparedNativeError::Unsupported(
            "single-entry thunk reentered"
        ))
    ));

    let mut failed = PreparedThunk::new(UpdatePolicy::Memoize);
    assert!(native.execute_memoized(&mut failed, &[]).is_err());
    assert!(matches!(
        native.execute_memoized(&mut failed, &[42]),
        Err(PreparedNativeError::Unsupported("memoized failure"))
    ));
}

#[test]
fn generated_descriptor_object_survives_registered_moving_collection() {
    let linked = linked_fixture();
    let native = PreparedNativeProgram::compile_binding(&linked, ValueId(17)).unwrap();
    let evidence = native.execute_after_registered_collection(&[42]).unwrap();
    assert!(evidence.root_moved);
    assert!(evidence.bytes_copied >= 16);
    assert_eq!(evidence.result.constructor.0, 1);
    assert_eq!(evidence.result.fields, vec![42]);
}

#[test]
fn unsupported_function_is_rejected_before_executable_publication() {
    let linked = linked_fixture();
    let error = match PreparedNativeProgram::compile_binding(&linked, ValueId(16)) {
        Ok(_) => panic!("recursive function unexpectedly compiled"),
        Err(error) => error,
    };
    assert!(matches!(error, PreparedNativeError::Unsupported(_)));
}
