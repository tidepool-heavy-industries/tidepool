use tidepool_bridge::Value;
use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, ImportedValue, MachineImports,
    ProgramRequirements, TargetDescriptor, ValueId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::DataConId;
use tidepool_runtime::prepared_execution::{
    run_prepared_once, PreparedCancelHandle, PreparedFailureKind, PreparedRuntimeError,
};
use tidepool_runtime::session::persistent::PreparedPersistentSession;

const ARTIFACT: &[u8] = include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor");

fn head(major: u8, length: usize) -> Vec<u8> {
    assert!(length < 24);
    vec![(major << 5) | length as u8]
}

fn array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let values: Vec<_> = values.into_iter().collect();
    let mut result = head(4, values.len());
    for value in values {
        result.extend(value);
    }
    result
}

fn uint(value: u64) -> Vec<u8> {
    if value <= 23 {
        vec![value as u8]
    } else if value <= u8::MAX as u64 {
        vec![0x18, value as u8]
    } else if value <= u16::MAX as u64 {
        let mut result = vec![0x19];
        result.extend((value as u16).to_be_bytes());
        result
    } else if value <= u32::MAX as u64 {
        let mut result = vec![0x1a];
        result.extend((value as u32).to_be_bytes());
        result
    } else {
        let mut result = vec![0x1b];
        result.extend(value.to_be_bytes());
        result
    }
}

fn text(value: &str) -> Vec<u8> {
    let mut result = head(3, value.len());
    result.extend(value.as_bytes());
    result
}

fn rep_lifted() -> Vec<u8> {
    array([uint(1)])
}

fn symbol(namespace: &str, module: &str, occurrence: &str) -> Vec<u8> {
    array([
        text("fixture"),
        text(module),
        text(namespace),
        text(occurrence),
    ])
}

fn strict_artifact() -> Vec<u8> {
    let constructor = array([
        symbol("value", "PreparedStrict", "Box"),
        symbol("type", "PreparedStrict", "BoxFamily"),
        array([]),
        array([]),
        array([array([]), uint(1), uint(0), array([])]),
        rep_lifted(),
        uint(1),
        uint(1),
        uint(100),
    ]);
    let expression = array([uint(4), uint(0), array([])]);
    let function = array([uint(0), uint(0), array([]), array([]), uint(0)]);
    let top = array([
        symbol("value", "PreparedStrict", "entry"),
        array([uint(0), function]),
    ]);
    let binding_group = array([uint(0), top]);
    array([
        text("TPSTG"),
        uint(SCHEMA_VERSION),
        text("ghc-9.12-prepared-stg"),
        text("ghc-9.12.2"),
        uint(EXECUTION_ABI_VERSION),
        array([
            uint(0),
            uint(0),
            uint(64),
            uint(64),
            text("sysv64"),
            array([]),
        ]),
        array([array([array([]), array([rep_lifted()])])]),
        array([]),
        array([constructor]),
        array([]),
        array([expression]),
        array([binding_group]),
        uint(0),
    ])
}

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
                    dead_end: global.dead_end,
                    evaluated: global.required_evaluated,
                    generation: global.required_generation.unwrap_or(0),
                };
                (value.identity.clone(), value)
            })
            .collect(),
    }
}

#[test]
fn one_shot_runs_closed_compiled_program_and_returns_values() {
    let cancel = PreparedCancelHandle::default();
    let result = run_prepared_once(
        &strict_artifact(),
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(result.collections, 0);
    assert!(matches!(
        result.values.as_slice(),
        [Value::Con(DataConId(100), fields)] if fields.is_empty()
    ));
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
fn retained_session_caches_closed_program_and_rejects_unclosed_artifact() {
    let mut session = PreparedPersistentSession::from_artifact(
        &strict_artifact(),
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .unwrap();
    let cancel = session.new_cancel_handle();
    let first = session
        .run_entry(Some(ValueId(0)), &[], true, &cancel)
        .unwrap();
    let second = session
        .run_entry(Some(ValueId(0)), &[], false, &cancel)
        .unwrap();
    assert!(matches!(
        first.values.as_slice(),
        [Value::Con(DataConId(100), fields)] if fields.is_empty()
    ));
    assert!(matches!(
        second.values.as_slice(),
        [Value::Con(DataConId(100), fields)] if fields.is_empty()
    ));
    assert_eq!(session.disposition(), MachineDisposition::Reusable);

    let mut unclosed = PreparedPersistentSession::from_artifact(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        imports(),
    )
    .unwrap();
    let unclosed_cancel = unclosed.new_cancel_handle();
    let rejected = unclosed
        .run_entry(None, &[], false, &unclosed_cancel)
        .unwrap_err();
    assert_eq!(rejected.kind(), PreparedFailureKind::Rejected);
    assert_eq!(unclosed.disposition(), MachineDisposition::Reusable);

    let mut session = PreparedPersistentSession::from_artifact(
        &strict_artifact(),
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .unwrap();
    let cancel = session.new_cancel_handle();
    let cancelled = session.new_cancel_handle();
    cancelled.cancel();
    assert!(matches!(
        session.run_entry(Some(ValueId(0)), &[], false, &cancelled),
        Err(PreparedRuntimeError::Cancelled)
    ));
    assert_eq!(session.disposition(), MachineDisposition::Reusable);
}
