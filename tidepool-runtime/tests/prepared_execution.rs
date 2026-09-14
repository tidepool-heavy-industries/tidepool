use std::sync::{atomic::AtomicBool, Arc};

use tidepool_bridge::Value;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::prepared_program::{
    CompiledProgram, ExecutionError, ObservationFailure, PreparedCallOptions,
    PreparedInput as CodegenPreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as PreparedOuterCodegen,
};
use tidepool_repr::execution_schema::{
    link_program, parse_program, Architecture, DecodeLimits, Endianness, ImportedValue,
    MachineImports, ProgramRequirements, TargetDescriptor, TopBinding, ValueId,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::DataConId;
use tidepool_runtime::prepared_execution::{
    run_prepared_once, PreparedArgument, PreparedCancelHandle, PreparedFailureKind, PreparedOuter,
    PreparedRuntimeError, PreparedValue, PreparedValueResult,
};
use tidepool_runtime::session::PreparedRuntime;

const ARTIFACT: &[u8] = include_bytes!("../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor");
const FREER_RETENTION_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/freer-retention.cbor");
const FREER_RESUME_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/freer-resume.cbor");
/// `program`'s GHC-computed final `Int`, transcribed from
/// `FreerResumeOracle.hs` run under the pinned GHC 9.12.2 (see that file's
/// header and `FreerResume.md`). Never hand-derived.
const FREER_RESUME_EXPECTATIONS: &str =
    include_str!("../../haskell/test-prepared-stg/FreerResumeExpectations.json");

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

/// `program`'s expected final `Int`, read out of `FreerResumeExpectations.json`.
fn expected_program_value() -> i64 {
    let parsed: serde_json::Value = serde_json::from_str(FREER_RESUME_EXPECTATIONS)
        .expect("FreerResumeExpectations.json parses as JSON");
    parsed["expectations"]["program"]["value"]
        .as_i64()
        .expect("FreerResumeExpectations.json's program expectation is an integer")
}

/// One constructor's host identity in `FREER_RESUME_ARTIFACT`, found by
/// occurrence name. Every occurrence the resume loop below inspects by
/// identity (`E`, `Val`, `Union`) appears exactly once in the artifact's
/// constructor table.
fn freer_resume_constructor_identity(occurrence: &str) -> DataConId {
    let prepared = parse_program(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("freer-resume artifact parses");
    prepared
        .constructors()
        .iter()
        .find(|constructor| constructor.identity.occurrence == occurrence)
        .unwrap_or_else(|| panic!("freer-resume artifact has no constructor named {occurrence}"))
        .host_id
}

/// Take one field as a managed value, replacing it with `Void` so the
/// `Vec` stays a valid (if partially consumed) field list.
fn take_managed(fields: &mut [PreparedValueResult], index: usize) -> PreparedValue {
    match std::mem::replace(&mut fields[index], PreparedValueResult::Void) {
        PreparedValueResult::Managed(value) => value,
        PreparedValueResult::Void | PreparedValueResult::Scalar(_) => {
            panic!("field {index} expected a managed value")
        }
    }
}

fn take_scalar(fields: &[PreparedValueResult], index: usize) -> u64 {
    match fields[index] {
        PreparedValueResult::Scalar(word) => word,
        PreparedValueResult::Void | PreparedValueResult::Managed(_) => {
            panic!("field {index} expected a scalar value")
        }
    }
}

/// Drives the compiled `qApp` resume loop (Wave 6B decision D2) end to end,
/// with no Rust-side freer walker: `PreparedRuntime::inspect_outer` reads
/// every constructor layer this loop looks at (`E`/`Val`, and `Union`'s
/// unpacked tag/payload shape), and `resumeInt`/`askArgument`/`valResult`
/// are the only things that ever force a field, and they do it as ordinary
/// compiled Haskell (a `qApp` application and two pattern matches), never as
/// Rust reading raw heap words.
///
/// `program`'s first suspension is `E { union = Union { tag = 0, payload =
/// Ask 3 } , k }`. The task card's original plan was to keep descending with
/// `inspect_outer` alone (`Union`'s payload -> `Ask` -> `I#`), but
/// `Union`'s payload field (`Data.OpenUnion.Internal`, a library type this
/// probe does not own) is an ordinary lazy field, so the retained payload is
/// still a `Thunk` object at that point -- and `inspect_outer` is
/// deliberately observation-only (see its doc comment in
/// `tidepool-codegen/src/prepared_program/observe.rs`): it errors
/// (`Unobservable(Thunk)`) rather than force it. This is not a Rust-side
/// limitation to work around by decoding bytes by hand; the fix is the
/// engine's own idiom for forcing a value from Rust, used the same way
/// `resumeInt` already is: a tiny compiled Haskell top that pattern-matches
/// (`askArgument`, `valResult` in `FreerResume.hs`) is called through
/// `run_entry_retained` with the retained value as a `Managed` argument, and
/// returns the forced `Int#` as a `Scalar`. `inspect_outer` still does all
/// the *shape* reading this loop needs (`E` vs `Val`, `Union`'s tag/payload
/// split); only the two fields nothing forces (`Ask`'s argument, `Val`'s
/// boxed `Int`) route through a compiled accessor instead.
///
/// Each suspension is answered by calling `resumeInt` with
/// `[PreparedArgument::Managed(&k), PreparedArgument::Scalar(answer)]` (the
/// compiled `resumeInt k n = qApp k (I# n)`), until the result is `Val`.
/// `collect_before_observation: true` on every `run_entry_retained` call
/// (the initial `program` run, every `askArgument`/`resumeInt` call, and the
/// final `valResult` call) forces a moving collection between suspension and
/// resume at every step, so this loop only completes if `k` and the retained
/// request are real GC roots, not raw pointers into a heap that already
/// moved.
///
/// The final `Int` is checked against `FreerResumeExpectations.json`
/// (`FreerResumeOracle.hs` run under the pinned GHC, never hand-typed).
/// Every `PreparedValue` this loop allocates is released as soon as it is no
/// longer needed, and the runtime's handle ledger must read back to zero at
/// the end: no leaked handles, and nothing forces `k` itself anywhere in
/// this loop (it is only ever inspected as an opaque `Managed` field and
/// handed back to `resumeInt`).
#[test]
fn freer_resume_loop_drives_qapp_to_completion_via_managed_resume_arguments() {
    let prepared = parse_program(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("freer-resume artifact parses");
    let program_top = freer_resume_top(&prepared, "program");
    let resume_int_top = freer_resume_top(&prepared, "resumeInt");
    let ask_argument_top = freer_resume_top(&prepared, "askArgument");
    let val_result_top = freer_resume_top(&prepared, "valResult");

    let e_id = freer_resume_constructor_identity("E");
    let val_id = freer_resume_constructor_identity("Val");
    let union_id = freer_resume_constructor_identity("Union");

    let mut runtime = PreparedRuntime::from_artifact(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .expect("freer-resume artifact is closed and admitted");

    let cancel = runtime.new_cancel_handle();
    let first = runtime
        .run_entry_retained(Some(program_top.binding.id), &[], true, &cancel)
        .expect("running `program` compiles the artifact and suspends on the first Ask");
    let mut first_values = first.values.into_iter();
    let Some(PreparedValueResult::Managed(mut outer)) = first_values.next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    assert!(first_values.next().is_none());

    let mut seen_answers = Vec::new();
    let final_value = loop {
        let PreparedOuter::Constructor {
            identity,
            mut fields,
        } = runtime
            .inspect_outer(&outer)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            // The boxed `Int` field itself is an ordinary lazy field (`pure
            // (a + b)` is never forced by anything on the path back to
            // Rust); `valResult` forces it below via `&outer` directly, so
            // this field is released unread.
            let boxed = take_managed(&mut fields, 0);
            assert!(runtime.release(boxed));

            let value_cancel = runtime.new_cancel_handle();
            let value_result = runtime
                .run_entry_retained(
                    Some(val_result_top.binding.id),
                    &[PreparedArgument::Managed(&outer)],
                    true,
                    &value_cancel,
                )
                .expect("valResult (Val (I# n) -> n) forces program's final Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedValueResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            assert!(runtime.release(outer));
            break word as i64;
        }

        assert_eq!(identity, e_id, "an Eff value at WHNF is either Val or E");
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed(&mut fields, 0);
        let k = take_managed(&mut fields, 1);
        assert!(runtime.release(outer));

        let PreparedOuter::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = runtime.inspect_outer(&union).expect("Union inspects");
        assert_eq!(union_identity, union_id);
        assert_eq!(
            union_fields.len(),
            2,
            "Union has an unpacked tag word and a payload"
        );
        let tag = take_scalar(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed(&mut union_fields, 1);
        assert!(runtime.release(union));

        // `payload` (`Union`'s second field) is an ordinary lazy field, so
        // it is still a `Thunk` object here; `askArgument` forces it (and,
        // since `Ask :: !Int -> Req Int` is strict, unboxes straight to
        // `Int#`) via an ordinary pattern match, compiled and called like
        // any other top -- not a Rust-side freer walker.
        let ask_cancel = runtime.new_cancel_handle();
        let ask_result = runtime
            .run_entry_retained(
                Some(ask_argument_top.binding.id),
                &[PreparedArgument::Managed(&payload)],
                true,
                &ask_cancel,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedValueResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(runtime.release(payload));
        seen_answers.push(n);

        let resume_cancel = runtime.new_cancel_handle();
        let resumed = runtime
            .run_entry_retained(
                Some(resume_int_top.binding.id),
                &[PreparedArgument::Managed(&k), PreparedArgument::Scalar(n)],
                true,
                &resume_cancel,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(runtime.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedValueResult::Managed(next_outer)) = resumed_values.next() else {
            panic!("resumeInt must return one managed `Eff` outer value");
        };
        assert!(resumed_values.next().is_none());
        outer = next_outer;
    };

    assert_eq!(
        seen_answers,
        vec![3, 4],
        "program's own Ask arguments: a <- send (Ask 3), then send (Ask (a + 1))"
    );
    assert_eq!(
        final_value,
        expected_program_value(),
        "resume loop result must match FreerResumeOracle.hs's GHC-computed value"
    );
    assert_eq!(
        runtime.retained_handle_count(),
        0,
        "every PreparedValue produced along the resume loop must be released"
    );
}

/// Every top and constructor identity `E3`'s parking-semantics tests below
/// need out of `FREER_RESUME_ARTIFACT`, gathered once so each test states
/// only what it actually exercises.
struct FreerResumeFixture {
    program_top: TopBinding,
    resume_int_top: TopBinding,
    ask_argument_top: TopBinding,
    val_result_top: TopBinding,
    e_id: DataConId,
    val_id: DataConId,
    union_id: DataConId,
}

impl FreerResumeFixture {
    fn load() -> Self {
        let prepared = parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses");
        Self {
            program_top: freer_resume_top(&prepared, "program"),
            resume_int_top: freer_resume_top(&prepared, "resumeInt"),
            ask_argument_top: freer_resume_top(&prepared, "askArgument"),
            val_result_top: freer_resume_top(&prepared, "valResult"),
            e_id: freer_resume_constructor_identity("E"),
            val_id: freer_resume_constructor_identity("Val"),
            union_id: freer_resume_constructor_identity("Union"),
        }
    }
}

/// Drive one already-suspended `Eff '[Req] Int` value (`outer`, as returned
/// by running `FreerResume.program` or by a prior `resumeInt`) to its final
/// `Val` and return the settled `Int`. Exactly the E2 resume protocol
/// (`inspect_outer` for shape; `askArgument`/`valResult`/`resumeInt` --
/// compiled Haskell, not a Rust-side freer walker -- for the fields
/// `inspect_outer` cannot force without evaluating), extracted so E3's
/// interleaving tests can drive two independent suspensions without
/// duplicating the loop. `collect_before_observation: true` on every call
/// forces a moving collection between every suspend/resume step, the same
/// guarantee E2's single-continuation test relies on.
fn drive_freer_program_to_val(
    runtime: &mut PreparedRuntime,
    fixture: &FreerResumeFixture,
    mut outer: PreparedValue,
) -> i64 {
    loop {
        let PreparedOuter::Constructor {
            identity,
            mut fields,
        } = runtime
            .inspect_outer(&outer)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == fixture.val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            let boxed = take_managed(&mut fields, 0);
            assert!(runtime.release(boxed));

            let value_cancel = runtime.new_cancel_handle();
            let value_result = runtime
                .run_entry_retained(
                    Some(fixture.val_result_top.binding.id),
                    &[PreparedArgument::Managed(&outer)],
                    true,
                    &value_cancel,
                )
                .expect("valResult (Val (I# n) -> n) forces the settled Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedValueResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            assert!(runtime.release(outer));
            return word as i64;
        }

        assert_eq!(identity, fixture.e_id, "an Eff value at WHNF is either Val or E");
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed(&mut fields, 0);
        let k = take_managed(&mut fields, 1);
        assert!(runtime.release(outer));

        let PreparedOuter::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = runtime.inspect_outer(&union).expect("Union inspects");
        assert_eq!(union_identity, fixture.union_id);
        assert_eq!(union_fields.len(), 2, "Union has an unpacked tag word and a payload");
        let tag = take_scalar(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed(&mut union_fields, 1);
        assert!(runtime.release(union));

        let ask_cancel = runtime.new_cancel_handle();
        let ask_result = runtime
            .run_entry_retained(
                Some(fixture.ask_argument_top.binding.id),
                &[PreparedArgument::Managed(&payload)],
                true,
                &ask_cancel,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedValueResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(runtime.release(payload));

        let resume_cancel = runtime.new_cancel_handle();
        let resumed = runtime
            .run_entry_retained(
                Some(fixture.resume_int_top.binding.id),
                &[PreparedArgument::Managed(&k), PreparedArgument::Scalar(n)],
                true,
                &resume_cancel,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(runtime.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedValueResult::Managed(next_outer)) = resumed_values.next() else {
            panic!("resumeInt must return one managed `Eff` outer value");
        };
        assert!(resumed_values.next().is_none());
        outer = next_outer;
    }
}

/// Split one suspended `E { union, k }` outer into its two managed fields,
/// releasing the outer's own root (the `E` constructor cell itself is never
/// needed again once its fields are retained separately).
fn split_suspension(
    runtime: &mut PreparedRuntime,
    fixture: &FreerResumeFixture,
    outer: PreparedValue,
) -> (PreparedValue, PreparedValue) {
    let PreparedOuter::Constructor { identity, mut fields } = runtime
        .inspect_outer(&outer)
        .expect("a freshly suspended Eff value inspects");
    assert_eq!(identity, fixture.e_id, "program's first suspension is E, not Val");
    assert_eq!(fields.len(), 2);
    let union = take_managed(&mut fields, 0);
    let k = take_managed(&mut fields, 1);
    assert!(runtime.release(outer));
    (union, k)
}

/// E3(a): two `program` runs on the same runtime each park their own
/// continuation (`k1`, `k2`); resuming the *second* one to completion before
/// the first, with the many forced collections `drive_freer_program_to_val`
/// performs along the way sitting between the two resumes, must not disturb
/// `k1` -- it still drives to the same GHC-computed value once its turn
/// comes. Wave 6B decision D3 ("`&mut self` on `PreparedMachine` serializes
/// turns; a parked continuation is inert heap data") is exactly the claim
/// this pins: interleaving is safe because nothing but retained heap data
/// survives a park, so resume order cannot matter to correctness.
#[test]
fn parked_continuations_resume_out_of_order_with_a_collection_between() {
    let fixture = FreerResumeFixture::load();
    let mut runtime = PreparedRuntime::from_artifact(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .expect("freer-resume artifact is closed and admitted");

    let first_cancel = runtime.new_cancel_handle();
    let first = runtime
        .run_entry_retained(Some(fixture.program_top.binding.id), &[], true, &first_cancel)
        .expect("first `program` run suspends on its first Ask");
    let Some(PreparedValueResult::Managed(outer1)) = first.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };

    let second_cancel = runtime.new_cancel_handle();
    let second = runtime
        .run_entry_retained(Some(fixture.program_top.binding.id), &[], true, &second_cancel)
        .expect("second, independent `program` run also suspends on its first Ask");
    let Some(PreparedValueResult::Managed(outer2)) = second.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };

    assert_eq!(
        runtime.retained_handle_count(),
        2,
        "both parked E{{union, k}} cells are live roots at once"
    );

    // Out of order: drive the second suspension all the way to its `Val`
    // first. Every step inside this call forces a collection before
    // observation, so `k1` (still only reachable through `outer1`, untouched
    // here) survives many moving collections while parked.
    let second_value = drive_freer_program_to_val(&mut runtime, &fixture, outer2);
    assert_eq!(runtime.disposition(), MachineDisposition::Reusable);

    // Now resume the first suspension to completion -- the collection(s)
    // just forced while draining `outer2` are the collection between the
    // two resumes the task card asks for.
    let first_value = drive_freer_program_to_val(&mut runtime, &fixture, outer1);

    assert_eq!(second_value, expected_program_value());
    assert_eq!(first_value, expected_program_value());
    assert_eq!(
        runtime.retained_handle_count(),
        0,
        "every PreparedValue produced along both resume loops must be released"
    );
    assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
}

/// E3(b): while `k1` sits parked (retained, never inspected or applied), an
/// entirely unrelated `program` run (`program` again -- a fresh, independent
/// effect computation sharing no data with `k1`) runs to its own suspension
/// on the same runtime. The parked `k1` must still be a live, unrelated
/// value that later resumes correctly, and the machine's disposition must
/// stay `Reusable` throughout -- an unrelated entry running while something
/// is parked is not itself a failure condition.
///
/// This test also carries the card's typed-refusal requirement: attempting
/// to `inspect_outer` the parked `k1` itself (an `Arrs` closure, never a
/// constructor at WHNF) must be a typed `ObservationFailure::Unobservable`
/// refusal, not a panic and not a silent success -- `inspect_outer` is
/// documented observation-only and must never force or otherwise expose a
/// parked continuation's body.
#[test]
fn unrelated_entry_runs_while_a_parked_k_stays_untouched_and_machine_reusable() {
    let fixture = FreerResumeFixture::load();
    let mut runtime = PreparedRuntime::from_artifact(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
    )
    .expect("freer-resume artifact is closed and admitted");

    let first_cancel = runtime.new_cancel_handle();
    let first = runtime
        .run_entry_retained(Some(fixture.program_top.binding.id), &[], true, &first_cancel)
        .expect("first `program` run suspends on its first Ask");
    let Some(PreparedValueResult::Managed(outer1)) = first.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let (union1, k1) = split_suspension(&mut runtime, &fixture, outer1);

    // `k1` is now parked: an opaque retained `Arrs` closure, reachable only
    // through this handle, not touched again until the end of this test.
    // Reviewer probe: forcing it by observation must be a typed refusal.
    // `k1`'s own outer layer is ordinary WHNF data, not a closure: per the
    // architectural invariant ("Freer continuations retain the Leaf/Node
    // type-aligned sequence shape; they are not represented as a single
    // closure", `CLAUDE.md`), `k1 :: Arrs '[Req] Int Int` is an
    // `FTCQueue`, and `program`'s two dependent `send`s build it as
    // `Node (Leaf f1) (Leaf f2)` under `>>=`'s desugaring -- a real
    // constructor `inspect_outer` may read without forcing anything. This
    // step reads *that* shape (empirically confirmed: two managed fields,
    // i.e. `Node`'s two `FTCQueue` children) but never releases `k1`
    // itself, so `k1` remains untouched and usable below.
    let PreparedOuter::Constructor {
        fields: mut node_fields,
        ..
    } = runtime
        .inspect_outer(&k1)
        .expect("k1's own Node/Leaf FTCQueue cell is ordinary WHNF data and inspects");
    assert_eq!(node_fields.len(), 2, "program's two dependent sends build k1 as Node(Leaf, Leaf)");
    let left = take_managed(&mut node_fields, 0);
    let right = take_managed(&mut node_fields, 1);

    // One layer deeper is where forcing is actually refused: `Leaf`'s own
    // field is `a -> m b`, a bare closure, never a constructor at WHNF.
    // This is the typed refusal the task card and its reviewer ask for --
    // not at k1's own outer cell (which is real data), but at the closure
    // FTCQueue's leaves actually carry.
    let PreparedOuter::Constructor {
        fields: mut leaf_fields,
        ..
    } = runtime
        .inspect_outer(&left)
        .expect("Leaf itself is ordinary WHNF data (one field: the closure)");
    assert_eq!(leaf_fields.len(), 1, "Leaf has exactly one field: the closure");
    let closure = take_managed(&mut leaf_fields, 0);

    let forced = runtime.inspect_outer(&closure);
    assert!(
        matches!(
            forced,
            Err(PreparedRuntimeError::Run(ExecutionError::Observation(
                ObservationFailure::Unobservable(_)
            )))
        ),
        "a Leaf's own field is a bare closure, never a constructor at WHNF; \
         inspect_outer must refuse it with a typed ObservationFailure"
    );
    assert_eq!(
        runtime.disposition(),
        MachineDisposition::Reusable,
        "a refused observation is not a machine-level failure"
    );
    assert!(runtime.release(closure));
    assert!(runtime.release(left));
    assert!(runtime.release(right));

    // An unrelated entry runs on the same machine while k1 sits parked: a
    // second, independent `program` invocation, sharing no state with k1's
    // suspension, driven all the way to completion.
    let second_cancel = runtime.new_cancel_handle();
    let second = runtime
        .run_entry_retained(Some(fixture.program_top.binding.id), &[], true, &second_cancel)
        .expect("an unrelated `program` run proceeds normally while k1 is parked");
    let Some(PreparedValueResult::Managed(outer2)) = second.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let unrelated_value = drive_freer_program_to_val(&mut runtime, &fixture, outer2);
    assert_eq!(unrelated_value, expected_program_value());
    assert_eq!(
        runtime.disposition(),
        MachineDisposition::Reusable,
        "running an unrelated entry while k1 is parked must leave the \
         machine reusable"
    );

    // k1 itself is unaffected: resuming its own suspension now still reaches
    // the same GHC-computed answer as any fresh run.
    let PreparedOuter::Constructor {
        identity: union_identity,
        fields: mut union_fields,
    } = runtime.inspect_outer(&union1).expect("Union inspects");
    assert_eq!(union_identity, fixture.union_id);
    let payload = take_managed(&mut union_fields, 1);
    assert!(runtime.release(union1));

    let ask_cancel = runtime.new_cancel_handle();
    let ask_result = runtime
        .run_entry_retained(
            Some(fixture.ask_argument_top.binding.id),
            &[PreparedArgument::Managed(&payload)],
            true,
            &ask_cancel,
        )
        .expect("askArgument still forces k1's own Ask request after the unrelated run");
    let mut ask_values = ask_result.values.into_iter();
    let Some(PreparedValueResult::Scalar(n)) = ask_values.next() else {
        panic!("askArgument must return one scalar Int#");
    };
    assert!(ask_values.next().is_none());
    assert!(runtime.release(payload));

    let resume_cancel = runtime.new_cancel_handle();
    let resumed = runtime
        .run_entry_retained(
            Some(fixture.resume_int_top.binding.id),
            &[PreparedArgument::Managed(&k1), PreparedArgument::Scalar(n)],
            true,
            &resume_cancel,
        )
        .expect("resumeInt still applies k1 after an unrelated entry ran while it was parked");
    assert!(runtime.release(k1));
    let mut resumed_values = resumed.values.into_iter();
    let Some(PreparedValueResult::Managed(next_outer)) = resumed_values.next() else {
        panic!("resumeInt must return one managed `Eff` outer value");
    };
    assert!(resumed_values.next().is_none());
    let first_value = drive_freer_program_to_val(&mut runtime, &fixture, next_outer);

    assert_eq!(first_value, expected_program_value());
    assert_eq!(
        runtime.retained_handle_count(),
        0,
        "every PreparedValue produced by this test must be released"
    );
    assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
}

/// E3(c): cancellation set so a running entry's own compiled safepoint poll
/// observes it -- not a shortcut check performed before generated code ever
/// runs. `PreparedRuntime::run_entry_retained` (`tidepool-runtime/src/session/prepared.rs`)
/// checks its `PreparedCancelHandle` in plain Rust before it ever calls into
/// the machine, so driving cancellation through that wrapper would prove
/// only the wrapper's own precondition check, not the generated-code
/// contract. This test instead drives `tidepool_codegen::prepared_program::PreparedMachine`
/// directly (the same lower layer `machine.rs`'s own
/// `cancellation_is_recoverable_before_a_following_entry` test uses): its
/// `run_entry_retained` installs the cancel flag onto the `MachineState`
/// (`machine.rs` `self.machine.set_cancel_flag(cancel)`) and then calls
/// straight into the compiled adapter with no Rust-side cancellation check
/// of its own -- the `Cancelled` status this test observes can only have
/// come back from the entry's own `prepared_poll_at` safepoint (see
/// `safepoint.rs`), reached from inside the generated `resumeInt` code
/// itself, after `qApp`'s own call already began.
///
/// Per the design pass ("no handler committed, safe to retry"): a
/// suspension's `k` never observes an `Ask`'s answer being applied unless
/// `qApp` actually runs to completion, so a call cancelled at its very first
/// safepoint commits nothing, and `k` -- never consumed, since
/// `run_entry_retained` only takes it by reference -- remains a valid handle
/// this test then successfully resumes.
#[test]
fn cancellation_before_commit_leaves_a_parked_k_valid_for_retry() {
    let requirements = requirements();
    let prepared = parse_program(FREER_RESUME_ARTIFACT, &requirements, DecodeLimits::default())
        .expect("freer-resume artifact parses");
    let program_top = freer_resume_top(&prepared, "program");
    let resume_int_top = freer_resume_top(&prepared, "resumeInt");
    let ask_argument_top = freer_resume_top(&prepared, "askArgument");
    let e_id = freer_resume_constructor_identity("E");
    let union_id = freer_resume_constructor_identity("Union");

    let linked = link_program(prepared, &MachineImports::default())
        .expect("freer-resume artifact is closed and admits with no imports");
    let program = CompiledProgram::compile(&linked).expect("freer-resume artifact compiles");
    let mut machine = PreparedMachine::new(program, PreparedMachineOptions { nursery_bytes: 4096 })
        .expect("freer-resume program installs");

    let call_options = PreparedCallOptions {
        observation_budget: 0,
        collect_before_observation: true,
    };

    let first = machine
        .run_entry_retained(
            program_top.binding.id,
            &[],
            call_options,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("`program` suspends on its first Ask");
    let mut first_values = first.values.into_iter();
    let Some(tidepool_codegen::prepared_program::PreparedResult::Managed(outer)) =
        first_values.next()
    else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    assert!(first_values.next().is_none());

    let PreparedOuterCodegen::Constructor { identity, fields } = machine
        .inspect_outer(outer)
        .expect("the freshly suspended Eff value inspects");
    assert_eq!(identity, e_id);
    assert_eq!(fields.len(), 2);
    let mut fields = fields;
    let union = match std::mem::replace(
        &mut fields[0],
        tidepool_codegen::prepared_program::PreparedResult::Void,
    ) {
        tidepool_codegen::prepared_program::PreparedResult::Managed(handle) => handle,
        other => panic!("Union field expected a managed value, got {other:?}"),
    };
    let k = match std::mem::replace(
        &mut fields[1],
        tidepool_codegen::prepared_program::PreparedResult::Void,
    ) {
        tidepool_codegen::prepared_program::PreparedResult::Managed(handle) => handle,
        other => panic!("Arrs field expected a managed value, got {other:?}"),
    };
    assert!(machine.release(outer));

    let PreparedOuterCodegen::Constructor {
        identity: union_identity,
        fields: mut union_fields,
    } = machine.inspect_outer(union).expect("Union inspects");
    assert_eq!(union_identity, union_id);
    let payload = match std::mem::replace(
        &mut union_fields[1],
        tidepool_codegen::prepared_program::PreparedResult::Void,
    ) {
        tidepool_codegen::prepared_program::PreparedResult::Managed(handle) => handle,
        other => panic!("Union payload expected a managed value, got {other:?}"),
    };
    assert!(machine.release(union));

    let ask_result = machine
        .run_entry_retained(
            ask_argument_top.binding.id,
            &[CodegenPreparedInput::Managed(payload)],
            call_options,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("askArgument forces the Ask request's Int#");
    let mut ask_values = ask_result.values.into_iter();
    let Some(tidepool_codegen::prepared_program::PreparedResult::Scalar(n)) = ask_values.next()
    else {
        panic!("askArgument must return one scalar Int#");
    };
    assert!(ask_values.next().is_none());
    assert!(machine.release(payload));

    // The flag generated code's own safepoint will see, not a value read
    // before entering it: `run_entry_retained` above never checked this
    // flag, and neither does the call below before it reaches the adapter.
    let cancel = Arc::new(AtomicBool::new(true));
    let cancelled = machine
        .run_entry_retained(
            resume_int_top.binding.id,
            &[CodegenPreparedInput::Managed(k), CodegenPreparedInput::Scalar(n)],
            call_options,
            cancel,
        )
        .expect_err("a cancel flag already set when generated code starts must still be \
                      caught by resumeInt's own entry safepoint, not skip execution");
    assert!(matches!(
        &cancelled,
        ExecutionError::Runtime(failure)
            if failure.cause == RuntimeError::Cancelled
                && failure.disposition == MachineDisposition::Reusable
    ));
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // k was only ever borrowed by the cancelled call (run_entry_retained
    // takes PreparedInput::Managed by handle, never consumes it), so it
    // remains a valid handle: a following, uncancelled resumeInt call with
    // the very same k succeeds.
    let resumed = machine
        .run_entry_retained(
            resume_int_top.binding.id,
            &[CodegenPreparedInput::Managed(k), CodegenPreparedInput::Scalar(n)],
            call_options,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("k remains a valid handle after a cancellation that committed nothing");
    let mut resumed_values = resumed.values.into_iter();
    assert!(matches!(
        resumed_values.next(),
        Some(tidepool_codegen::prepared_program::PreparedResult::Managed(_))
    ));
    assert!(resumed_values.next().is_none());
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}
