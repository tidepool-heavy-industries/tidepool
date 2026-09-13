use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, ImportedValue, MachineImports,
    ProgramRequirements, TargetDescriptor, ValueId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_runtime::prepared_execution::{
    run_prepared_once, PreparedCancelHandle, PreparedFailureKind, PreparedRuntimeError,
};
use tidepool_runtime::session::persistent::PreparedPersistentSession;

const ARTIFACT: &[u8] = include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor");

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

fn imports() -> MachineImports {
    let prepared = parse_program(ARTIFACT, &requirements(), DecodeLimits::default()).unwrap();
    MachineImports {
        values: prepared
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
            .collect(),
    }
}

#[test]
fn one_shot_canonical_artifact_runs_direct_native_collection() {
    let cancel = PreparedCancelHandle::default();
    let result = run_prepared_once(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        imports(),
        &cancel,
    )
    .unwrap();
    assert_eq!(result.value.constructor.0, 0);
    assert_eq!(result.value.fields, vec![42]);
    let collection = result.collection.unwrap();
    assert!(collection.root_moved);
    assert!(collection.bytes_copied >= 16);
}

#[test]
fn one_shot_rejects_missing_import_malformed_and_precancel() {
    let cancel = PreparedCancelHandle::default();
    let missing = run_prepared_once(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
        &cancel,
    )
    .unwrap_err();
    assert_eq!(missing.kind(), PreparedFailureKind::Rejected);

    let malformed = run_prepared_once(
        &[0xff],
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
        &cancel,
    )
    .unwrap_err();
    assert_eq!(malformed.kind(), PreparedFailureKind::Rejected);

    cancel.cancel();
    let cancelled = run_prepared_once(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        imports(),
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(cancelled, PreparedRuntimeError::Cancelled));
    assert_eq!(cancelled.kind(), PreparedFailureKind::Cancelled);
}

#[test]
fn retained_session_reuses_program_and_preserves_disposition() {
    let mut session = PreparedPersistentSession::from_artifact(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        imports(),
    )
    .unwrap();
    let cancel = session.new_cancel_handle();
    let first = session
        .run_entry(Some(ValueId(17)), &[42], true, &cancel)
        .unwrap();
    let second = session
        .run_entry(Some(ValueId(17)), &[99], true, &cancel)
        .unwrap();
    assert_eq!(first.value.fields, vec![42]);
    assert_eq!(second.value.fields, vec![99]);
    assert_eq!(session.disposition(), MachineDisposition::Reusable);

    let rejected = session
        .run_entry(Some(ValueId(16)), &[], false, &cancel)
        .unwrap_err();
    assert_eq!(rejected.kind(), PreparedFailureKind::Rejected);
    assert_eq!(session.disposition(), MachineDisposition::Reusable);

    let cancelled = session.new_cancel_handle();
    cancelled.cancel();
    assert!(matches!(
        session.run_entry(Some(ValueId(17)), &[1], false, &cancelled),
        Err(PreparedRuntimeError::Cancelled)
    ));
    assert_eq!(session.disposition(), MachineDisposition::Reusable);
}
