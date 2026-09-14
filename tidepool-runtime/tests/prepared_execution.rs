use tidepool_bridge::Value;
use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, ImportedValue, MachineImports,
    ProgramRequirements, TargetDescriptor, ValueId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::DataConId;
use tidepool_runtime::prepared_execution::{
    run_prepared_once, PreparedCancelHandle, PreparedFailureKind, PreparedOuter,
    PreparedRuntimeError, PreparedValueResult,
};
use tidepool_runtime::session::PreparedRuntime;

const ARTIFACT: &[u8] = include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor");
const FREER_RETENTION_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/freer-retention.cbor");
const FREER_RESUME_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/freer-resume.cbor");

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
        array([uint(0)]),
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
        array([array([array([]), array([uint(0), array([rep_lifted()])])])]),
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
                    evaluated: global.required_evaluated,
                    generation: global.required_generation.unwrap_or(0),
                };
                (value.identity.clone(), value)
            })
            .collect(),
    }
}

fn freer_effect_identity() -> DataConId {
    let prepared = parse_program(
        FREER_RETENTION_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("FreerRetention artifact parses");
    prepared
        .constructors()
        .iter()
        .find(|constructor| constructor.identity.occurrence == "E")
        .expect("FreerRetention artifact includes the real freer E constructor")
        .host_id
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
    // `run_prepared_once` requests the contract's collection-before-observation
    // checkpoint, so this closed constructor run performs one ordinary GC.
    assert_eq!(result.collections, 1);
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
    let mut session = PreparedRuntime::from_artifact(
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

    let mut unclosed = PreparedRuntime::from_artifact(
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

    let mut session = PreparedRuntime::from_artifact(
        &strict_artifact(),
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .unwrap();
    let cancelled = session.new_cancel_handle();
    cancelled.cancel();
    assert!(matches!(
        session.run_entry(Some(ValueId(0)), &[], false, &cancelled),
        Err(PreparedRuntimeError::Cancelled)
    ));
    assert_eq!(session.disposition(), MachineDisposition::Reusable);
}

#[test]
fn runtime_retains_a_real_freer_continuation_without_observing_it() {
    let mut runtime = PreparedRuntime::from_artifact(
        FREER_RETENTION_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .expect("FreerRetention artifact is closed and admitted");
    let first_cancel = runtime.new_cancel_handle();
    let first = runtime
        .run_entry_retained(None, &[], false, &first_cancel)
        .expect("first Freer request is retained");
    let mut first_values = first.values.into_iter();
    let Some(PreparedValueResult::Managed(outer)) = first_values.next() else {
        panic!("Freer request must return one managed outer value");
    };
    assert!(first_values.next().is_none());

    let second_cancel = runtime.new_cancel_handle();
    let second = runtime
        .run_entry_retained(None, &[], true, &second_cancel)
        .expect("a later collection retains the first Freer request");
    assert!(second.collections >= 1);
    let mut second_values = second.values.into_iter();
    let Some(PreparedValueResult::Managed(second_outer)) = second_values.next() else {
        panic!("second Freer request must return one managed outer value");
    };
    assert!(second_values.next().is_none());

    let PreparedOuter::Constructor { identity, fields } = runtime
        .inspect_outer(&outer)
        .expect("retained outer request survives the later collection");
    assert_eq!(identity, freer_effect_identity());
    let mut children: Vec<_> = fields
        .into_iter()
        .filter_map(|field| match field {
            PreparedValueResult::Managed(value) => Some(value),
            PreparedValueResult::Void | PreparedValueResult::Scalar(_) => None,
        })
        .collect();
    let Some(continuation) = children.pop() else {
        panic!("the real E continuation remains an opaque managed child");
    };
    assert!(runtime.release(outer));
    assert!(runtime.release(continuation));
    for child in children {
        assert!(runtime.release(child));
    }
    assert!(runtime.release(second_outer));
}

/// One `TopBinding` (with its enclosing group's recursion flattened) matching
/// `occurrence` in `FREER_RESUME_ARTIFACT`'s home module. Panics rather than
/// returning `Option` because every caller below treats a missing top as a
/// probe-shape failure, not a runtime condition to branch on.
fn freer_resume_top(
    prepared: &tidepool_repr::execution_schema::PreparedProgram,
    occurrence: &str,
) -> tidepool_repr::execution_schema::TopBinding {
    prepared
        .bindings()
        .iter()
        .flat_map(|group| match group {
            tidepool_repr::execution_schema::Group::NonRecursive(top) => {
                std::slice::from_ref(top).to_vec()
            }
            tidepool_repr::execution_schema::Group::Recursive(tops) => tops.clone(),
        })
        .find(|top| top.identity.module == "FreerResume" && top.identity.occurrence == occurrence)
        .unwrap_or_else(|| panic!("FreerResume artifact has no top named {occurrence}"))
}

/// `program` and `resumeInt` do not reference each other's top-level
/// bindings (`resumeInt` is not in the reachability closure the corpus
/// projection computes for `program`'s entry, and vice versa), so
/// `freerResumeEntries = (program, resumeInt)` is projected as the artifact's
/// designated entry purely to pull both into one reachable closure. Neither
/// `program` nor `resumeInt` needs to be the *designated* entry to be a real,
/// separately callable top of this artifact: `run_entry`/`run_entry_retained`
/// accept any top's `ValueId` directly (see
/// `retained_session_caches_closed_program_and_rejects_unclosed_artifact`
/// above), and admission is whole-program
/// (`tidepool_codegen::prepared_program::admission::admit_prepared`), so a
/// single successful compile admits every top in the artifact, `resumeInt`
/// included.
#[test]
fn freer_resume_artifact_admits_program_and_resume_int_as_two_entries() {
    let prepared = parse_program(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("freer-resume artifact parses");

    let program_top = freer_resume_top(&prepared, "program");
    let resume_int_top = freer_resume_top(&prepared, "resumeInt");

    let resume_int_signature = match &resume_int_top.binding.rhs {
        tidepool_repr::execution_schema::HeapRhs::Function { signature, .. } => {
            &prepared.signatures()[signature.0 as usize]
        }
        other => panic!("resumeInt must project as a callable function top, got {other:?}"),
    };
    assert_eq!(
        resume_int_signature.arguments,
        vec![
            tidepool_repr::execution_schema::RuntimeRep::LiftedRef,
            tidepool_repr::execution_schema::RuntimeRep::Int(64)
        ],
        "resumeInt k n = qApp k (I# n) takes the retained Arrs continuation \
         and the unboxed Int# answer, with no Void state-token argument \
         because Eff is not IO"
    );

    // `PreparedRuntime::from_artifact` parses and links; compilation (and
    // therefore whole-program admission) is deferred to the first entry run.
    let mut runtime = PreparedRuntime::from_artifact(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .expect("freer-resume artifact is closed and links with no missing imports");

    let cancel = runtime.new_cancel_handle();
    let program_run = runtime
        .run_entry_retained(Some(program_top.binding.id), &[], false, &cancel)
        .expect(
            "running the `program` top compiles (and whole-program-admits) \
             the artifact, which includes `resumeInt` as a second top",
        );
    assert_eq!(program_run.values.len(), 1);
    assert!(matches!(
        program_run.values[0],
        PreparedValueResult::Managed(_)
    ));

    // `admit_prepared` (`tidepool_codegen::prepared_program::admission`) is
    // documented whole-program: it walks every top in `program.bindings()`
    // before any entry compiles, not just the one about to run. The
    // successful `program` run above therefore already proves `resumeInt`'s
    // entry ABI was admitted alongside it. This test stops short of calling
    // `resumeInt` itself: doing so needs a real retained `Arrs` continuation
    // (a `Managed` argument, not a fabricated `Scalar`), and building one is
    // E2's resume-loop concern, not E1's admission probe.
}
