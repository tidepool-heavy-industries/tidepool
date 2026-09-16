use std::sync::{atomic::AtomicBool, Arc};

use tidepool_bridge::Value;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::prepared_program::{
    CompiledProgram, ExecutionError, ImportBindings, ObservationFailure, PreparedCallOptions,
    PreparedHandle, PreparedInput as CodegenPreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as PreparedOuterCodegen, PreparedResult, ProgramId, RunOptions, RunResult,
};
use tidepool_repr::execution_schema::{
    link_program, parse_program, Architecture, DecodeLimits, Endianness, ImportedValue, LinkError,
    MachineImports, PreparedProgram, ProgramRequirements, SymbolIdentity, TargetDescriptor,
    TopBinding, ValueId, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::freer_names::{
    find_declared, E_DEFINING_MODULE, UNION_DEFINING_MODULE, VAL_DEFINING_MODULE,
};
use tidepool_repr::DataConId;
use tidepool_runtime::prepared_execution::{PreparedFailureKind, PreparedRuntimeError, RealmId};

/// This file drives `tidepool_codegen::prepared_program::PreparedMachine`
/// directly: the deleted `PreparedRuntime` session-bookkeeping wrapper
/// (bindings/generations/leases) duplicated the real production owner,
/// `PreparedEngine` (`tidepool-runtime/src/session/prepared.rs`), and none of
/// the tests below exercise session-level bookkeeping -- they are all
/// machine-level (parking, resuming, realms, cancellation, freer-resume
/// loops, cross-program imports, GC), so they are ported onto the lower
/// layer instead of onto `PreparedEngine`.
///
/// One-shot convenience equivalent to the deleted
/// `tidepool_runtime::prepared_execution::run_prepared_once`: parse, link,
/// compile, install on a fresh ephemeral machine, and run the artifact's
/// designated entry with no arguments, using the same default nursery size
/// and observation budget the deleted wrapper used
/// ([`RunOptions::default`]).
fn run_prepared_once(
    artifact: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
    imports: MachineImports,
    cancel: Arc<AtomicBool>,
) -> Result<RunResult, PreparedRuntimeError> {
    // A caller may pre-cancel before this function even parses the artifact
    // (e.g. a request already cancelled before compilation started); a
    // trivial entry may never reach a tail-call safepoint that would
    // otherwise observe `cancel`, so this up-front check is load-bearing,
    // not redundant with `run_entry_with_raw_cancel`'s own safepoint checks.
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(PreparedRuntimeError::Cancelled);
    }
    let prepared = parse_program(artifact, requirements, limits)?;
    let entry = prepared.entry();
    let linked = link_program(prepared, &imports)?;
    let compiled = CompiledProgram::compile(&linked).map_err(PreparedRuntimeError::Compile)?;
    let (mut machine, program) = PreparedMachine::new(
        compiled,
        PreparedMachineOptions {
            nursery_bytes: RunOptions::default().nursery_bytes,
        },
    )
    .map_err(PreparedRuntimeError::Run)?;
    let options = PreparedCallOptions {
        observation_budget: RunOptions::default().observation_budget,
        collect_before_observation: true,
    };
    machine
        .run_entry_with_raw_cancel(program, entry, &[], options, cancel)
        .map_err(|error| match &error {
            ExecutionError::Runtime(failure) if failure.cause == RuntimeError::Cancelled => {
                PreparedRuntimeError::Cancelled
            }
            _ => PreparedRuntimeError::Run(error),
        })
}

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

#[test]
fn caller_result_matches_ghc_for_boxed_unboxed_and_join_forwarding() {
    use tidepool_extract_cmd::{resolve_bin, ExtractCmd, ResolvedExtractBin};

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let source = root.join("haskell/test-prepared-stg/RepPoly.hs");
    let oracle = std::process::Command::new("ghc")
        .arg(&source)
        .args(["-e", "print [RepPoly.result, RepPoly.joined]"])
        .output()
        .expect("run the pinned GHC oracle in the repository development shell");
    assert!(
        oracle.status.success(),
        "{}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    let expected: Vec<i64> = serde_json::from_slice(&oracle.stdout).expect("GHC Int list");
    assert_eq!(expected.len(), 2);

    let output = tempfile::tempdir().unwrap();
    let mut command = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(
        resolve_bin()
            .expect("resolve the repository extractor")
            .path,
    ));
    command
        .input(&source)
        .targets(["result", "joined"])
        .include(root.join("haskell/lib"))
        .output_dir(output.path());
    let extracted = command
        .bind()
        .and_then(|endpoint| endpoint.execute(&command))
        .expect("extract CallerResult programs");
    assert!(
        extracted.output.status.success(),
        "{}",
        String::from_utf8_lossy(&extracted.output.stderr)
    );

    let requirements = tidepool_toolchain::prepared_artifact::production_requirements()
        .expect("resolve the production artifact requirements");
    for (target, expected) in ["result", "joined"].into_iter().zip(expected) {
        let bytes = std::fs::read(output.path().join(format!("{target}.prepared.cbor")))
            .expect("extractor wrote the prepared artifact");
        let result = run_prepared_once(
            &bytes,
            &requirements,
            DecodeLimits::default(),
            MachineImports::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_or_else(|error| panic!("{target}: {error:?}"));
        assert_eq!(result.values.len(), 1, "{target}");
        assert_eq!(observed_int(&result.values[0]), expected, "{target}");
    }
}

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
        array([]),
        array([]),
    ])
}

fn strict_program() -> PreparedProgram {
    parse_program(&strict_artifact(), &requirements(), DecodeLimits::default())
        .expect("strict_artifact() parses")
}

/// The default [`PreparedCallOptions`] `PreparedRuntime`'s deleted
/// convenience wrappers used everywhere: the same observation budget
/// [`RunOptions::default`] carries, with `collect_before_observation` set
/// per call site.
fn call_options(collect_before_observation: bool) -> PreparedCallOptions {
    PreparedCallOptions {
        observation_budget: RunOptions::default().observation_budget,
        collect_before_observation,
    }
}

fn machine_options() -> PreparedMachineOptions {
    PreparedMachineOptions {
        nursery_bytes: RunOptions::default().nursery_bytes,
    }
}

/// A fresh machine with `strict_program()` installed as its only, closed
/// program -- the direct-`PreparedMachine` equivalent of
/// `PreparedRuntime::from_artifact(&strict_artifact(), ...)`.
fn open_strict_machine() -> (PreparedMachine<'static>, ProgramId) {
    let linked =
        link_program(strict_program(), &MachineImports::default()).expect("strict links closed");
    let compiled = CompiledProgram::compile(&linked).expect("strict artifact compiles");
    PreparedMachine::new(compiled, machine_options()).expect("strict artifact installs")
}

/// Link `prepared` with no imports, compile and install it as a fresh
/// machine's only program -- the direct-`PreparedMachine` equivalent of
/// `PreparedRuntime::from_prepared(prepared, MachineImports::default())` for
/// a closed artifact.
fn open_closed_machine_from(prepared: PreparedProgram) -> (PreparedMachine<'static>, ProgramId) {
    let linked = link_program(prepared, &MachineImports::default()).expect("artifact links closed");
    let compiled = CompiledProgram::compile(&linked).expect("artifact compiles");
    PreparedMachine::new(compiled, machine_options()).expect("artifact installs")
}

/// Parse, link with no imports, compile and install `artifact` as a fresh
/// machine's only program, returning its own designated entry alongside --
/// the direct-`PreparedMachine` equivalent of
/// `PreparedRuntime::from_artifact(artifact, ..., MachineImports::default())`
/// for a closed artifact whose tests drive the designated entry (what the
/// deleted wrapper's `run_entry(None, ...)` meant).
fn open_closed_machine(artifact: &[u8]) -> (PreparedMachine<'static>, ProgramId, ValueId) {
    let prepared =
        parse_program(artifact, &requirements(), DecodeLimits::default()).expect("artifact parses");
    let entry = prepared.entry();
    let linked = link_program(prepared, &MachineImports::default()).expect("artifact links closed");
    let compiled = CompiledProgram::compile(&linked).expect("artifact compiles");
    let (machine, program) =
        PreparedMachine::new(compiled, machine_options()).expect("artifact installs");
    (machine, program, entry)
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
                    rep: global.rep,
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
    find_declared(prepared.constructors(), E_DEFINING_MODULE, "E")
        .expect("FreerRetention artifact includes the real freer E constructor")
        .host_id
}

#[test]
fn one_shot_runs_closed_compiled_program_and_returns_values() {
    let cancel = Arc::new(AtomicBool::new(false));
    let result = run_prepared_once(
        &strict_artifact(),
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
        cancel,
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
    let cancel = Arc::new(AtomicBool::new(false));
    let missing = run_prepared_once(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
        Arc::clone(&cancel),
    )
    .unwrap_err();
    assert_eq!(missing.kind(), PreparedFailureKind::Rejected);

    let malformed = run_prepared_once(
        &[0xff],
        &requirements(),
        DecodeLimits::default(),
        MachineImports::default(),
        Arc::clone(&cancel),
    )
    .unwrap_err();
    assert_eq!(malformed.kind(), PreparedFailureKind::Rejected);

    cancel.store(true, std::sync::atomic::Ordering::Release);
    let cancelled = run_prepared_once(
        ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
        imports(),
        Arc::clone(&cancel),
    )
    .unwrap_err();
    assert!(matches!(cancelled, PreparedRuntimeError::Cancelled));
    assert_eq!(cancelled.kind(), PreparedFailureKind::Cancelled);
}

#[test]
fn retained_session_caches_closed_program_and_rejects_unclosed_artifact() {
    let (mut machine, closed_program) = open_strict_machine();
    let first = machine
        .run_entry(
            closed_program,
            ValueId(0),
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .unwrap();
    let second = machine
        .run_entry(
            closed_program,
            ValueId(0),
            &[],
            call_options(false),
            RealmId::ROOT,
        )
        .unwrap();
    assert!(matches!(
        first.values.as_slice(),
        [Value::Con(DataConId(100), fields)] if fields.is_empty()
    ));
    assert!(matches!(
        second.values.as_slice(),
        [Value::Con(DataConId(100), fields)] if fields.is_empty()
    ));
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // `imports()` builds a `MachineImports` whose facts satisfy `ARTIFACT`'s
    // own declared globals exactly (so `link_program` succeeds), but no real
    // `PreparedHandle` backs any of those identities: `install_program`
    // refuses with `ExecutionError::UnknownPreparedHandle`, the same
    // `PreparedFailureKind::Rejected` classification the deleted
    // `PreparedRuntime::run_entry(None, ...)` surfaced for an artifact whose
    // declared imports were never actually bound. Installing it as a SECOND
    // program on an already-open, already-working machine (rather than as
    // the very first program) lets this test also observe that the refusal
    // leaves that machine's own disposition untouched.
    let (mut unclosed_machine, _first) = open_strict_machine();
    let unclosed_linked = link_program(
        parse_program(ARTIFACT, &requirements(), DecodeLimits::default()).unwrap(),
        &imports(),
    )
    .expect("ARTIFACT's declared globals satisfy imports()'s fabricated facts");
    let unclosed_compiled = unclosed_machine
        .compile_for_install(&unclosed_linked)
        .expect("ARTIFACT compiles");
    let rejected = unclosed_machine
        .install_program(unclosed_compiled, ImportBindings::new())
        .unwrap_err();
    assert!(
        matches!(rejected, ExecutionError::UnknownPreparedHandle),
        "an unclosed artifact is refused, not run: {rejected:?}"
    );
    assert_eq!(unclosed_machine.disposition(), MachineDisposition::Reusable);

    let (mut cancel_machine, cancel_program) = open_strict_machine();
    let cancelled_realm = RealmId::fresh();
    cancel_machine.realm_cancel_handle(cancelled_realm).cancel();
    let cancelled = cancel_machine.run_entry(
        cancel_program,
        ValueId(0),
        &[],
        call_options(false),
        cancelled_realm,
    );
    assert!(matches!(
        &cancelled,
        Err(ExecutionError::Runtime(failure)) if failure.cause == RuntimeError::Cancelled
    ));
    assert_eq!(cancel_machine.disposition(), MachineDisposition::Reusable);
}

#[test]
fn runtime_retains_a_real_freer_continuation_without_observing_it() {
    let (mut machine, program, entry) = open_closed_machine(FREER_RETENTION_ARTIFACT);
    let first = machine
        .run_entry_retained(program, entry, &[], call_options(false), RealmId::ROOT)
        .expect("first Freer request is retained");
    let mut first_values = first.values.into_iter();
    let Some(PreparedResult::Managed(outer)) = first_values.next() else {
        panic!("Freer request must return one managed outer value");
    };
    assert!(first_values.next().is_none());

    let second = machine
        .run_entry_retained(program, entry, &[], call_options(true), RealmId::ROOT)
        .expect("a later collection retains the first Freer request");
    assert!(second.collections >= 1);
    let mut second_values = second.values.into_iter();
    let Some(PreparedResult::Managed(second_outer)) = second_values.next() else {
        panic!("second Freer request must return one managed outer value");
    };
    assert!(second_values.next().is_none());

    let PreparedOuterCodegen::Constructor { identity, fields } = machine
        .inspect_outer(outer, RealmId::ROOT)
        .expect("retained outer request survives the later collection");
    assert_eq!(identity, freer_effect_identity());
    let mut children: Vec<_> = fields
        .into_iter()
        .filter_map(|field| match field {
            PreparedResult::Managed(value) => Some(value),
            PreparedResult::Void | PreparedResult::Scalar(_) => None,
        })
        .collect();
    let Some(continuation) = children.pop() else {
        panic!("the real E continuation remains an opaque managed child");
    };
    assert!(machine.release(outer));
    assert!(machine.release(continuation));
    for child in children {
        assert!(machine.release(child));
    }
    assert!(machine.release(second_outer));
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

    // The deleted `PreparedRuntime::from_artifact` parsed and linked eagerly
    // but deferred compilation (and therefore whole-program admission) to
    // the first entry run; `PreparedMachine::new` below compiles and admits
    // the whole artifact up front instead, so this test's own compile step
    // already stands in for that deferred-admission moment.
    let (mut machine, program) = open_closed_machine_from(
        parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses"),
    );

    let program_run = machine
        .run_entry_retained(
            program,
            program_top.binding.id,
            &[],
            call_options(false),
            RealmId::ROOT,
        )
        .expect(
            "running the `program` top compiles (and whole-program-admits) \
             the artifact, which includes `resumeInt` as a second top",
        );
    assert_eq!(program_run.values.len(), 1);
    assert!(matches!(program_run.values[0], PreparedResult::Managed(_)));

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

/// Resolve a freer constructor from the artifact by its defining module.
fn freer_resume_constructor_identity(module: &str, occurrence: &str) -> DataConId {
    let prepared = parse_program(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("freer-resume artifact parses");
    find_declared(prepared.constructors(), module, occurrence)
        .unwrap_or_else(|| panic!("freer-resume artifact has no constructor named {occurrence}"))
        .host_id
}

/// Take one field as a managed value, replacing it with `Void` so the
/// `Vec` stays a valid (if partially consumed) field list.
fn take_managed(fields: &mut [PreparedResult], index: usize) -> PreparedHandle {
    match std::mem::replace(&mut fields[index], PreparedResult::Void) {
        PreparedResult::Managed(value) => value,
        PreparedResult::Void | PreparedResult::Scalar(_) => {
            panic!("field {index} expected a managed value")
        }
    }
}

fn take_scalar(fields: &[PreparedResult], index: usize) -> u64 {
    match fields[index] {
        PreparedResult::Scalar(word) => word,
        PreparedResult::Void | PreparedResult::Managed(_) => {
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

    let e_id = freer_resume_constructor_identity(E_DEFINING_MODULE, "E");
    let val_id = freer_resume_constructor_identity(VAL_DEFINING_MODULE, "Val");
    let union_id = freer_resume_constructor_identity(UNION_DEFINING_MODULE, "Union");

    let (mut machine, program) = open_closed_machine_from(prepared);

    let first = machine
        .run_entry_retained(
            program,
            program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("running `program` compiles the artifact and suspends on the first Ask");
    let mut first_values = first.values.into_iter();
    let Some(PreparedResult::Managed(mut outer)) = first_values.next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    assert!(first_values.next().is_none());

    let mut seen_answers = Vec::new();
    let final_value = loop {
        let PreparedOuterCodegen::Constructor {
            identity,
            mut fields,
        } = machine
            .inspect_outer(outer, RealmId::ROOT)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            // The boxed `Int` field itself is an ordinary lazy field (`pure
            // (a + b)` is never forced by anything on the path back to
            // Rust); `valResult` forces it below via `outer` directly, so
            // this field is released unread.
            let boxed = take_managed(&mut fields, 0);
            assert!(machine.release(boxed));

            let value_result = machine
                .run_entry_retained(
                    program,
                    val_result_top.binding.id,
                    &[CodegenPreparedInput::Managed(outer)],
                    call_options(true),
                    RealmId::ROOT,
                )
                .expect("valResult (Val (I# n) -> n) forces program's final Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            assert!(machine.release(outer));
            break word as i64;
        }

        assert_eq!(identity, e_id, "an Eff value at WHNF is either Val or E");
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed(&mut fields, 0);
        let k = take_managed(&mut fields, 1);
        assert!(machine.release(outer));

        let PreparedOuterCodegen::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = machine
            .inspect_outer(union, RealmId::ROOT)
            .expect("Union inspects");
        assert_eq!(union_identity, union_id);
        assert_eq!(
            union_fields.len(),
            2,
            "Union has an unpacked tag word and a payload"
        );
        let tag = take_scalar(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed(&mut union_fields, 1);
        assert!(machine.release(union));

        // `payload` (`Union`'s second field) is an ordinary lazy field, so
        // it is still a `Thunk` object here; `askArgument` forces it (and,
        // since `Ask :: !Int -> Req Int` is strict, unboxes straight to
        // `Int#`) via an ordinary pattern match, compiled and called like
        // any other top -- not a Rust-side freer walker.
        let ask_result = machine
            .run_entry_retained(
                program,
                ask_argument_top.binding.id,
                &[CodegenPreparedInput::Managed(payload)],
                call_options(true),
                RealmId::ROOT,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(machine.release(payload));
        seen_answers.push(n);

        let resumed = machine
            .run_entry_retained(
                program,
                resume_int_top.binding.id,
                &[
                    CodegenPreparedInput::Managed(k),
                    CodegenPreparedInput::Scalar(n),
                ],
                call_options(true),
                RealmId::ROOT,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(machine.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedResult::Managed(next_outer)) = resumed_values.next() else {
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
        machine.handle_count(),
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
            e_id: freer_resume_constructor_identity(E_DEFINING_MODULE, "E"),
            val_id: freer_resume_constructor_identity(VAL_DEFINING_MODULE, "Val"),
            union_id: freer_resume_constructor_identity(UNION_DEFINING_MODULE, "Union"),
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
    machine: &mut PreparedMachine<'static>,
    program: ProgramId,
    fixture: &FreerResumeFixture,
    mut outer: PreparedHandle,
) -> i64 {
    loop {
        let PreparedOuterCodegen::Constructor {
            identity,
            mut fields,
        } = machine
            .inspect_outer(outer, RealmId::ROOT)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == fixture.val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            let boxed = take_managed(&mut fields, 0);
            assert!(machine.release(boxed));

            let value_result = machine
                .run_entry_retained(
                    program,
                    fixture.val_result_top.binding.id,
                    &[CodegenPreparedInput::Managed(outer)],
                    call_options(true),
                    RealmId::ROOT,
                )
                .expect("valResult (Val (I# n) -> n) forces the settled Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            assert!(machine.release(outer));
            return word as i64;
        }

        assert_eq!(
            identity, fixture.e_id,
            "an Eff value at WHNF is either Val or E"
        );
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed(&mut fields, 0);
        let k = take_managed(&mut fields, 1);
        assert!(machine.release(outer));

        let PreparedOuterCodegen::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = machine
            .inspect_outer(union, RealmId::ROOT)
            .expect("Union inspects");
        assert_eq!(union_identity, fixture.union_id);
        assert_eq!(
            union_fields.len(),
            2,
            "Union has an unpacked tag word and a payload"
        );
        let tag = take_scalar(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed(&mut union_fields, 1);
        assert!(machine.release(union));

        let ask_result = machine
            .run_entry_retained(
                program,
                fixture.ask_argument_top.binding.id,
                &[CodegenPreparedInput::Managed(payload)],
                call_options(true),
                RealmId::ROOT,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(machine.release(payload));

        let resumed = machine
            .run_entry_retained(
                program,
                fixture.resume_int_top.binding.id,
                &[
                    CodegenPreparedInput::Managed(k),
                    CodegenPreparedInput::Scalar(n),
                ],
                call_options(true),
                RealmId::ROOT,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(machine.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedResult::Managed(next_outer)) = resumed_values.next() else {
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
    machine: &mut PreparedMachine<'static>,
    fixture: &FreerResumeFixture,
    outer: PreparedHandle,
) -> (PreparedHandle, PreparedHandle) {
    let PreparedOuterCodegen::Constructor {
        identity,
        mut fields,
    } = machine
        .inspect_outer(outer, RealmId::ROOT)
        .expect("a freshly suspended Eff value inspects");
    assert_eq!(
        identity, fixture.e_id,
        "program's first suspension is E, not Val"
    );
    assert_eq!(fields.len(), 2);
    let union = take_managed(&mut fields, 0);
    let k = take_managed(&mut fields, 1);
    assert!(machine.release(outer));
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
    let (mut machine, program) = open_closed_machine_from(
        parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses"),
    );

    let first = machine
        .run_entry_retained(
            program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("first `program` run suspends on its first Ask");
    let Some(PreparedResult::Managed(outer1)) = first.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };

    let second = machine
        .run_entry_retained(
            program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("second, independent `program` run also suspends on its first Ask");
    let Some(PreparedResult::Managed(outer2)) = second.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };

    assert_eq!(
        machine.handle_count(),
        2,
        "both parked E{{union, k}} cells are live roots at once"
    );

    // Out of order: drive the second suspension all the way to its `Val`
    // first. Every step inside this call forces a collection before
    // observation, so `k1` (still only reachable through `outer1`, untouched
    // here) survives many moving collections while parked.
    let second_value = drive_freer_program_to_val(&mut machine, program, &fixture, outer2);
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // Now resume the first suspension to completion -- the collection(s)
    // just forced while draining `outer2` are the collection between the
    // two resumes the task card asks for.
    let first_value = drive_freer_program_to_val(&mut machine, program, &fixture, outer1);

    assert_eq!(second_value, expected_program_value());
    assert_eq!(first_value, expected_program_value());
    assert_eq!(
        machine.handle_count(),
        0,
        "every PreparedValue produced along both resume loops must be released"
    );
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
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
    let (mut machine, program) = open_closed_machine_from(
        parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses"),
    );

    let first = machine
        .run_entry_retained(
            program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("first `program` run suspends on its first Ask");
    let Some(PreparedResult::Managed(outer1)) = first.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let (union1, k1) = split_suspension(&mut machine, &fixture, outer1);

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
    let PreparedOuterCodegen::Constructor {
        fields: mut node_fields,
        ..
    } = machine
        .inspect_outer(k1, RealmId::ROOT)
        .expect("k1's own Node/Leaf FTCQueue cell is ordinary WHNF data and inspects");
    assert_eq!(
        node_fields.len(),
        2,
        "program's two dependent sends build k1 as Node(Leaf, Leaf)"
    );
    let left = take_managed(&mut node_fields, 0);
    let right = take_managed(&mut node_fields, 1);

    // One layer deeper is where forcing is actually refused: `Leaf`'s own
    // field is `a -> m b`, a bare closure, never a constructor at WHNF.
    // This is the typed refusal the task card and its reviewer ask for --
    // not at k1's own outer cell (which is real data), but at the closure
    // FTCQueue's leaves actually carry.
    let PreparedOuterCodegen::Constructor {
        fields: mut leaf_fields,
        ..
    } = machine
        .inspect_outer(left, RealmId::ROOT)
        .expect("Leaf itself is ordinary WHNF data (one field: the closure)");
    assert_eq!(
        leaf_fields.len(),
        1,
        "Leaf has exactly one field: the closure"
    );
    let closure = take_managed(&mut leaf_fields, 0);

    let forced = machine.inspect_outer(closure, RealmId::ROOT);
    assert!(
        matches!(
            forced,
            Err(ExecutionError::Observation(
                ObservationFailure::Unobservable(_)
            ))
        ),
        "a Leaf's own field is a bare closure, never a constructor at WHNF; \
         inspect_outer must refuse it with a typed ObservationFailure"
    );
    assert_eq!(
        machine.disposition(),
        MachineDisposition::Reusable,
        "a refused observation is not a machine-level failure"
    );
    assert!(machine.release(closure));
    assert!(machine.release(left));
    assert!(machine.release(right));

    // An unrelated entry runs on the same machine while k1 sits parked: a
    // second, independent `program` invocation, sharing no state with k1's
    // suspension, driven all the way to completion.
    let second = machine
        .run_entry_retained(
            program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("an unrelated `program` run proceeds normally while k1 is parked");
    let Some(PreparedResult::Managed(outer2)) = second.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let unrelated_value = drive_freer_program_to_val(&mut machine, program, &fixture, outer2);
    assert_eq!(unrelated_value, expected_program_value());
    assert_eq!(
        machine.disposition(),
        MachineDisposition::Reusable,
        "running an unrelated entry while k1 is parked must leave the \
         machine reusable"
    );

    // k1 itself is unaffected: resuming its own suspension now still reaches
    // the same GHC-computed answer as any fresh run.
    let PreparedOuterCodegen::Constructor {
        identity: union_identity,
        fields: mut union_fields,
    } = machine
        .inspect_outer(union1, RealmId::ROOT)
        .expect("Union inspects");
    assert_eq!(union_identity, fixture.union_id);
    let payload = take_managed(&mut union_fields, 1);
    assert!(machine.release(union1));

    let ask_result = machine
        .run_entry_retained(
            program,
            fixture.ask_argument_top.binding.id,
            &[CodegenPreparedInput::Managed(payload)],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("askArgument still forces k1's own Ask request after the unrelated run");
    let mut ask_values = ask_result.values.into_iter();
    let Some(PreparedResult::Scalar(n)) = ask_values.next() else {
        panic!("askArgument must return one scalar Int#");
    };
    assert!(ask_values.next().is_none());
    assert!(machine.release(payload));

    let resumed = machine
        .run_entry_retained(
            program,
            fixture.resume_int_top.binding.id,
            &[
                CodegenPreparedInput::Managed(k1),
                CodegenPreparedInput::Scalar(n),
            ],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("resumeInt still applies k1 after an unrelated entry ran while it was parked");
    assert!(machine.release(k1));
    let mut resumed_values = resumed.values.into_iter();
    let Some(PreparedResult::Managed(next_outer)) = resumed_values.next() else {
        panic!("resumeInt must return one managed `Eff` outer value");
    };
    assert!(resumed_values.next().is_none());
    let first_value = drive_freer_program_to_val(&mut machine, program, &fixture, next_outer);

    assert_eq!(first_value, expected_program_value());
    assert_eq!(
        machine.handle_count(),
        0,
        "every PreparedValue produced by this test must be released"
    );
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}

/// E3(c): cancellation set so a running entry's own compiled safepoint poll
/// observes it -- not a shortcut check performed before generated code ever
/// runs. `PreparedRuntime::run_entry_retained` (`tidepool-runtime/src/session/prepared.rs`)
/// checks its realm's cancel flag in plain Rust before it ever calls into
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
    let prepared = parse_program(
        FREER_RESUME_ARTIFACT,
        &requirements,
        DecodeLimits::default(),
    )
    .expect("freer-resume artifact parses");
    let program_top = freer_resume_top(&prepared, "program");
    let resume_int_top = freer_resume_top(&prepared, "resumeInt");
    let ask_argument_top = freer_resume_top(&prepared, "askArgument");
    let e_id = freer_resume_constructor_identity(E_DEFINING_MODULE, "E");
    let union_id = freer_resume_constructor_identity(UNION_DEFINING_MODULE, "Union");

    let linked = link_program(prepared, &MachineImports::default())
        .expect("freer-resume artifact is closed and admits with no imports");
    let program = CompiledProgram::compile(&linked).expect("freer-resume artifact compiles");
    let (mut machine, program_id) = PreparedMachine::new(
        program,
        PreparedMachineOptions {
            nursery_bytes: 4096,
        },
    )
    .expect("freer-resume program installs");

    let call_options = PreparedCallOptions {
        observation_budget: 0,
        collect_before_observation: true,
    };

    let first = machine
        .run_entry_retained(
            program_id,
            program_top.binding.id,
            &[],
            call_options,
            tidepool_codegen::suspension::RealmId::ROOT,
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
        .inspect_outer(outer, tidepool_codegen::suspension::RealmId::ROOT)
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
    } = machine
        .inspect_outer(union, tidepool_codegen::suspension::RealmId::ROOT)
        .expect("Union inspects");
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
            program_id,
            ask_argument_top.binding.id,
            &[CodegenPreparedInput::Managed(payload)],
            call_options,
            tidepool_codegen::suspension::RealmId::ROOT,
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
    let cancel_realm = tidepool_codegen::suspension::RealmId::fresh();
    machine.realm_cancel_handle(cancel_realm).cancel();
    let cancelled = machine
        .run_entry_retained(
            program_id,
            resume_int_top.binding.id,
            &[
                CodegenPreparedInput::Managed(k),
                CodegenPreparedInput::Scalar(n),
            ],
            call_options,
            cancel_realm,
        )
        .expect_err(
            "a cancel flag already set when generated code starts must still be \
                      caught by resumeInt's own entry safepoint, not skip execution",
        );
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
            program_id,
            resume_int_top.binding.id,
            &[
                CodegenPreparedInput::Managed(k),
                CodegenPreparedInput::Scalar(n),
            ],
            call_options,
            tidepool_codegen::suspension::RealmId::ROOT,
        )
        .expect("k remains a valid handle after a cancellation that committed nothing");
    let mut resumed_values = resumed.values.into_iter();
    assert!(matches!(
        resumed_values.next(),
        Some(tidepool_codegen::prepared_program::PreparedResult::Managed(
            _
        ))
    ));
    assert!(resumed_values.next().is_none());
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}

/// [`take_managed`], but for the raw `PreparedMachine` API's own
/// `PreparedResult`, used by the direct-`PreparedMachine` cancellation
/// tests below instead of `PreparedRuntime`'s `PreparedValueResult`.
fn take_managed_result(fields: &mut [PreparedResult], index: usize) -> PreparedHandle {
    match std::mem::replace(&mut fields[index], PreparedResult::Void) {
        PreparedResult::Managed(handle) => handle,
        other => panic!("field {index} expected a managed value, got {other:?}"),
    }
}

/// [`take_scalar`], but for [`PreparedResult`].
fn take_scalar_result(fields: &[PreparedResult], index: usize) -> u64 {
    match fields[index] {
        PreparedResult::Scalar(word) => word,
        other => panic!("field {index} expected a scalar value, got {other:?}"),
    }
}

/// [`drive_freer_program_to_val_in`]'s own loop, but against the raw
/// `PreparedMachine`/`ProgramId` primitives (the same layer
/// `cancellation_before_commit_leaves_a_parked_k_valid_for_retry` drives
/// directly) and tagged with an explicit `realm`, so the two-realm
/// cancellation test below can drive each realm's own suspension without
/// going through `PreparedRuntime`'s realm-agnostic convenience wrapper.
/// Every intermediate root this loop mints under `realm` (`union`, `k`,
/// `payload`) is released as soon as it is consumed, exactly as
/// `drive_freer_program_to_val_in` does; unlike that helper, the settled
/// `Val` cell itself is handed back to the caller UNRELEASED, so a caller
/// that wants to keep proving the settled value is still reachable through
/// `realm` after some unrelated event (here, a sibling realm's
/// `close_realm`) can do so before releasing it.
fn drive_direct_to_val(
    machine: &mut PreparedMachine,
    program_id: ProgramId,
    call_options: PreparedCallOptions,
    fixture: &FreerResumeFixture,
    realm: RealmId,
    mut outer: PreparedHandle,
) -> (i64, PreparedHandle) {
    loop {
        let PreparedOuterCodegen::Constructor {
            identity,
            mut fields,
        } = machine
            .inspect_outer(outer, realm)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == fixture.val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            let boxed = take_managed_result(&mut fields, 0);
            assert!(machine.release(boxed));

            let value_result = machine
                .run_entry_retained(
                    program_id,
                    fixture.val_result_top.binding.id,
                    &[CodegenPreparedInput::Managed(outer)],
                    call_options,
                    realm,
                )
                .expect("valResult (Val (I# n) -> n) forces the settled Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            return (word as i64, outer);
        }

        assert_eq!(
            identity, fixture.e_id,
            "an Eff value at WHNF is either Val or E"
        );
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed_result(&mut fields, 0);
        let k = take_managed_result(&mut fields, 1);
        assert!(machine.release(outer));

        let PreparedOuterCodegen::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = machine.inspect_outer(union, realm).expect("Union inspects");
        assert_eq!(union_identity, fixture.union_id);
        assert_eq!(
            union_fields.len(),
            2,
            "Union has an unpacked tag word and a payload"
        );
        let tag = take_scalar_result(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed_result(&mut union_fields, 1);
        assert!(machine.release(union));

        let ask_result = machine
            .run_entry_retained(
                program_id,
                fixture.ask_argument_top.binding.id,
                &[CodegenPreparedInput::Managed(payload)],
                call_options,
                realm,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(machine.release(payload));

        let resumed = machine
            .run_entry_retained(
                program_id,
                fixture.resume_int_top.binding.id,
                &[
                    CodegenPreparedInput::Managed(k),
                    CodegenPreparedInput::Scalar(n),
                ],
                call_options,
                realm,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(machine.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedResult::Managed(next_outer)) = resumed_values.next() else {
            panic!("resumeInt must return one managed `Eff` outer value");
        };
        assert!(resumed_values.next().is_none());
        outer = next_outer;
    }
}

/// C1 follow-up: `cancellation_before_commit_leaves_a_parked_k_valid_for_retry`
/// above pins realm-scoped cancellation for exactly ONE realm on ONE
/// installed program; the task card that landed it deliberately scoped that
/// coverage down to a single realm and left two-realm independence as
/// follow-up work. This test closes that gap: TWO fresh, independent
/// realms (`r1`, `r2`) share ONE `PreparedMachine` and ONE installed
/// program, each parking its own continuation from the same freer-resume
/// artifact, with cancel/reset/resume/`close_realm` exercised across both.
///
/// Per that same test's own reasoning (repeated here because it is exactly
/// what makes this a real cancellation proof rather than a precondition
/// check): `PreparedMachine::run_entry_retained` installs the ACTIVE call's
/// cancel flag onto the shared `MachineState` and then calls straight into
/// the compiled adapter with no Rust-side cancellation check of its own --
/// the `Cancelled` status this test observes for `r1` can only have come
/// back from `resumeInt`'s own `prepared_poll_at` safepoint, reached from
/// inside the generated code that call actually starts. `r1`'s cancel flag
/// is set only AFTER `k1` is already parked and its Ask answer already
/// known (not before the scenario begins), so there is no way the flag
/// could have been observed before generated code for that specific call
/// started running. Throughout, `r2`'s own parked continuation, its
/// resume-to-completion, and its settled value are proven untouched by any
/// of `r1`'s cancellation, reset, or eventual `close_realm`.
#[test]
fn two_realms_share_one_machine_cancel_reset_close_independently_of_each_other() {
    let fixture = FreerResumeFixture::load();
    let prepared = parse_program(
        FREER_RESUME_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("freer-resume artifact parses");
    let linked = link_program(prepared, &MachineImports::default())
        .expect("freer-resume artifact is closed and admits with no imports");
    let program = CompiledProgram::compile(&linked).expect("freer-resume artifact compiles");
    let (mut machine, program_id) = PreparedMachine::new(
        program,
        PreparedMachineOptions {
            nursery_bytes: 4096,
        },
    )
    .expect("freer-resume program installs");

    let call_options = PreparedCallOptions {
        observation_budget: 0,
        collect_before_observation: true,
    };

    let r1 = RealmId::fresh();
    let r2 = RealmId::fresh();

    // Park one continuation per realm on the SAME installed program: run
    // `program` to its first suspension once tagged r1, once tagged r2.
    // `collect_before_observation: true` on every call below (including
    // both of these) forces a moving collection between the two runs and
    // at every following step, the same "collection between" guarantee
    // `parked_continuations_resume_out_of_order_with_a_collection_between`
    // relies on for two parked continuations within one realm.
    let first_r1 = machine
        .run_entry_retained(
            program_id,
            fixture.program_top.binding.id,
            &[],
            call_options,
            r1,
        )
        .expect("r1's `program` run suspends on its first Ask");
    let mut first_r1_values = first_r1.values.into_iter();
    let Some(PreparedResult::Managed(outer_r1)) = first_r1_values.next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    assert!(first_r1_values.next().is_none());

    let first_r2 = machine
        .run_entry_retained(
            program_id,
            fixture.program_top.binding.id,
            &[],
            call_options,
            r2,
        )
        .expect("r2's `program` run suspends on its first Ask");
    let mut first_r2_values = first_r2.values.into_iter();
    let Some(PreparedResult::Managed(outer_r2)) = first_r2_values.next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    assert!(first_r2_values.next().is_none());
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // Split r1's parked suspension into its own union/k -- k1 is now parked
    // and its Ask answer (n1) already forced, exactly the state
    // `cancellation_before_commit_leaves_a_parked_k_valid_for_retry` reaches
    // before it sets its own cancel flag.
    let PreparedOuterCodegen::Constructor {
        identity: id_r1,
        fields: mut fields_r1,
    } = machine
        .inspect_outer(outer_r1, r1)
        .expect("r1's suspended Eff value inspects");
    assert_eq!(id_r1, fixture.e_id);
    assert_eq!(fields_r1.len(), 2);
    let union_r1 = take_managed_result(&mut fields_r1, 0);
    let k1 = take_managed_result(&mut fields_r1, 1);
    assert!(machine.release(outer_r1));

    let PreparedOuterCodegen::Constructor {
        identity: union_id_r1,
        fields: mut union_fields_r1,
    } = machine
        .inspect_outer(union_r1, r1)
        .expect("r1's Union inspects");
    assert_eq!(union_id_r1, fixture.union_id);
    let payload_r1 = take_managed_result(&mut union_fields_r1, 1);
    assert!(machine.release(union_r1));

    let ask_r1 = machine
        .run_entry_retained(
            program_id,
            fixture.ask_argument_top.binding.id,
            &[CodegenPreparedInput::Managed(payload_r1)],
            call_options,
            r1,
        )
        .expect("askArgument forces r1's Ask request's Int#");
    let mut ask_r1_values = ask_r1.values.into_iter();
    let Some(PreparedResult::Scalar(n1)) = ask_r1_values.next() else {
        panic!("askArgument must return one scalar Int#");
    };
    assert!(ask_r1_values.next().is_none());
    assert!(machine.release(payload_r1));

    // Cancel ONLY r1, and only now -- after k1 is already parked. r2's own
    // cancel flag (never requested) stays clear.
    machine.realm_cancel_handle(r1).cancel();

    let cancelled = machine
        .run_entry_retained(
            program_id,
            fixture.resume_int_top.binding.id,
            &[
                CodegenPreparedInput::Managed(k1),
                CodegenPreparedInput::Scalar(n1),
            ],
            call_options,
            r1,
        )
        .expect_err(
            "r1's cancel flag, set before this call starts, must still be caught by \
             resumeInt's own entry safepoint, not skip execution",
        );
    assert!(matches!(
        &cancelled,
        ExecutionError::Runtime(failure)
            if failure.cause == RuntimeError::Cancelled
                && failure.disposition == MachineDisposition::Reusable
    ));
    assert_eq!(
        machine.disposition(),
        MachineDisposition::Reusable,
        "a cancelled call alone must never poison the machine"
    );

    // A cancelled call is that call's own outcome, not a machine failure:
    // an observation on the OTHER realm right after it must see r2's parked
    // `Eff` value, never a stale `Cancelled` read back from r1's call.
    // (`MachineState` keeps the reusable outcome and the Unavailable latch
    // in separate slots; only the latch outlives a call.)
    let PreparedOuterCodegen::Constructor {
        identity: id_r2_check,
        fields: mut fields_r2_check,
    } = machine
        .inspect_outer(outer_r2, r2)
        .expect("r2's parked Eff inspects cleanly right after r1's cancelled call");
    assert_eq!(id_r2_check, fixture.e_id);
    assert_eq!(fields_r2_check.len(), 2);
    let union_r2_check = take_managed_result(&mut fields_r2_check, 0);
    let k2_check = take_managed_result(&mut fields_r2_check, 1);
    assert!(machine.release(union_r2_check));
    assert!(machine.release(k2_check));
    assert_eq!(
        machine.failure(),
        None,
        "cancellation never reaches the machine latch"
    );

    // Reset r1 and retry with the very same `k1`/`n1`: the retry succeeds,
    // proving `k1` was never consumed by the cancelled call (the same proof
    // `cancellation_before_commit_leaves_a_parked_k_valid_for_retry` relies
    // on).
    machine.realm_cancel_handle(r1).reset();
    let resumed_r1 = machine
        .run_entry_retained(
            program_id,
            fixture.resume_int_top.binding.id,
            &[
                CodegenPreparedInput::Managed(k1),
                CodegenPreparedInput::Scalar(n1),
            ],
            call_options,
            r1,
        )
        .expect("k1 remains valid after a cancellation that committed nothing, and now proceeds");
    let mut resumed_r1_values = resumed_r1.values.into_iter();
    let Some(PreparedResult::Managed(next_outer_r1)) = resumed_r1_values.next() else {
        panic!("resumeInt must return one managed `Eff` outer value");
    };
    assert!(resumed_r1_values.next().is_none());

    // Drive r1 the rest of the way (program's second Ask still remains) to
    // its own settled `Val`.
    let (value_r1, outer_r1_final) = drive_direct_to_val(
        &mut machine,
        program_id,
        call_options,
        &fixture,
        r1,
        next_outer_r1,
    );
    assert_eq!(value_r1, expected_program_value());
    // r1's own settled cell is fully drained now: release it immediately,
    // so the only handle left under r1 is k1 itself, never explicitly
    // released above.
    assert!(machine.release(outer_r1_final));
    assert_eq!(
        machine.disposition(),
        MachineDisposition::Reusable,
        "r1's cancel/reset/resume history alone must never poison the machine"
    );

    // r2 was wholly unaffected by any of r1's cancel/reset/resume history
    // above: drive it all the way to its own settled `Val` now, keeping
    // that settled cell's handle alive (not released yet) so it can be
    // re-checked after r1's close_realm below.
    let (value_r2, outer_r2_final) = drive_direct_to_val(
        &mut machine,
        program_id,
        call_options,
        &fixture,
        r2,
        outer_r2,
    );
    assert_eq!(
        value_r2, value_r1,
        "r1 and r2 run the same deterministic computation from the same fixture"
    );

    // SCOPE EXIT: close r1. k1 is the only handle this test left live under
    // r1 (every other r1 root above was released as soon as it was
    // consumed), so close_realm must release exactly one handle. Frames are
    // always 0 for this engine: `PreparedMachine::close_realm`'s own doc
    // comment (`tidepool-codegen/src/prepared_program/machine.rs`) says the
    // prepared engine never parks a continuation in the frame-ledger sense,
    // so `frames_closed` cannot be anything but 0 here or for any prepared
    // program.
    let (frames_closed, handles_closed) = machine.close_realm(r1);
    assert_eq!(
        (frames_closed, handles_closed),
        (0, 1),
        "closing r1 must release exactly k1 -- the one r1 handle this test left live"
    );

    // r2's own settled cell is untouched by r1's close_realm: it still
    // resolves through r2, exactly as before.
    let PreparedOuterCodegen::Constructor {
        identity: id_check,
        fields: mut check_fields,
    } = machine
        .inspect_outer(outer_r2_final, r2)
        .expect("r2's settled Val cell still resolves through its own realm after r1's close");
    assert_eq!(id_check, fixture.val_id);
    assert_eq!(check_fields.len(), 1);
    let boxed_r2 = take_managed_result(&mut check_fields, 0);

    // Idempotent: closing an already-closed (or never-populated) realm
    // releases nothing.
    assert_eq!(machine.close_realm(r1), (0, 0));

    // Clean up r2's own remaining handles and confirm nothing leaked
    // anywhere on the machine.
    assert!(machine.release(boxed_r2));
    assert!(machine.release(outer_r2_final));
    assert_eq!(
        machine.handle_count(),
        0,
        "every handle either realm produced has been released or closed"
    );
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}

// ---- C0: rung 3 pinned across two installed programs --------------------
//
// E3 (`parked_continuations_resume_out_of_order_with_a_collection_between`,
// `unrelated_entry_runs_while_a_parked_k_stays_untouched_and_machine_reusable`)
// already pins interleaved parked work WITHIN one installed program. Rung 3
// as stated in the acceptance ladder is about two installed programs
// sharing one heap. This section installs the freer-resume artifact a
// second time on the SAME machine (`install_program`, no imports -- the two
// copies share no data) and drives both through `drive_freer_program_to_val`,
// which already takes an explicit [`ProgramId`] (the deleted
// `PreparedRuntime::run_entry_retained_in`'s whole reason to exist, folded
// directly into the one direct-`PreparedMachine` helper above rather than
// kept as a second, now-identical copy).

/// Parse, link with no imports, and install `artifact` as a SECOND (or
/// later) program on `machine` -- the direct-`PreparedMachine` equivalent of
/// the deleted `PreparedRuntime::install`.
fn install_closed(machine: &mut PreparedMachine<'static>, artifact: &[u8]) -> ProgramId {
    let prepared =
        parse_program(artifact, &requirements(), DecodeLimits::default()).expect("artifact parses");
    let linked = link_program(prepared, &MachineImports::default()).expect("artifact links closed");
    let compiled = machine
        .compile_for_install(&linked)
        .expect("artifact compiles for install");
    machine
        .install_program(compiled, ImportBindings::new())
        .expect("artifact installs")
}

/// Install the freer-resume artifact a second time on `machine` (no
/// imports: the two installed programs share no data, only the heap and
/// machinery), run its `program` entry to first suspension, and return the
/// program id with the parked outer value.
fn park_second_program(
    machine: &mut PreparedMachine<'static>,
    fixture: &FreerResumeFixture,
) -> (ProgramId, PreparedHandle) {
    let program = install_closed(machine, FREER_RESUME_ARTIFACT);
    let result = machine
        .run_entry_retained(
            program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("the second program's own `program` run suspends on its first Ask");
    let Some(PreparedResult::Managed(outer)) = result.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    (program, outer)
}

/// Rung 3, stated: two installed PROGRAMS (not just two runs of one
/// program) sharing one heap, resumed out of order, surviving a collection
/// between. Park `k` from program A (the session's first program) and `k`
/// from program B (installed here); drive B to completion first, then A,
/// with B's own collections sitting between the two resumes -- A's `k`
/// must survive every one of them untouched, exactly as within-program E3
/// already proved for a single program's two parked continuations.
#[test]
fn c0_two_installed_programs_park_and_resume_out_of_order_with_a_collection_between() {
    let fixture = FreerResumeFixture::load();
    let (mut machine, program_a) = open_closed_machine_from(
        parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses"),
    );

    let a_first = machine
        .run_entry_retained(
            program_a,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("program A's `program` run suspends on its first Ask");
    let Some(PreparedResult::Managed(outer_a)) = a_first.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };

    let (program_b, outer_b) = park_second_program(&mut machine, &fixture);
    assert_ne!(
        program_a, program_b,
        "the second install is a distinct program on the same machine"
    );
    assert_eq!(
        machine.handle_count(),
        2,
        "both programs' parked E{{union, k}} cells are live roots at once"
    );

    // Out of order: drive B to its `Val` first. Every step forces a
    // collection before observation, so A's `k` -- reachable only through
    // `outer_a`, on a DIFFERENT installed program, untouched here -- must
    // survive every one of B's collections while parked.
    let value_b = drive_freer_program_to_val(&mut machine, program_b, &fixture, outer_b);
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    let value_a = drive_freer_program_to_val(&mut machine, program_a, &fixture, outer_a);

    assert_eq!(value_b, expected_program_value());
    assert_eq!(value_a, expected_program_value());
    assert_eq!(
        machine.handle_count(),
        0,
        "every PreparedValue produced along both resume loops must be released"
    );
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}

/// The second half of rung 3's statement: while program A's `k` sits
/// parked, an ENTIRELY UNRELATED entry of program B (B's own `program`
/// entry, run to its own fresh suspension, never touching A's parked
/// value) runs on the same machine. A's parked `k` must remain a live,
/// untouched value that still resumes correctly afterward, and the
/// machine's disposition must stay `Reusable` throughout.
#[test]
fn c0_unrelated_entry_of_a_second_installed_program_runs_while_the_first_stays_parked() {
    let fixture = FreerResumeFixture::load();
    let (mut machine, program_a) = open_closed_machine_from(
        parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses"),
    );

    let a_first = machine
        .run_entry_retained(
            program_a,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            RealmId::ROOT,
        )
        .expect("program A's `program` run suspends on its first Ask");
    let Some(PreparedResult::Managed(outer_a)) = a_first.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    assert_eq!(machine.handle_count(), 1);

    // B installs and runs its own unrelated `program` entry to its own
    // fresh suspension, sharing no data with A's parked continuation.
    let (program_b, outer_b) = park_second_program(&mut machine, &fixture);
    assert_eq!(
        machine.handle_count(),
        2,
        "A's parked k plus B's own fresh suspension are both live roots"
    );
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // A's parked k must still be a valid, opaque continuation. k1 (an
    // `Arrs`/FTCQueue) is itself ordinary `Node(Leaf, Leaf)` data and
    // inspects; the typed refusal is one layer deeper, at a Leaf's own
    // field (a bare closure, never a constructor at WHNF) -- the same
    // shape `unrelated_entry_runs_while_a_parked_k_stays_untouched_...`
    // (single-program E3) already established for this fixture's k.
    let PreparedOuterCodegen::Constructor {
        fields: outer_a_fields,
        ..
    } = machine
        .inspect_outer(outer_a, RealmId::ROOT)
        .expect("A's parked E{union, k} outer still inspects");
    let mut outer_a_fields = outer_a_fields.into_iter();
    let Some(PreparedResult::Managed(union_a)) = outer_a_fields.next() else {
        panic!("E's first field must be the managed Union");
    };
    assert!(machine.release(union_a));
    let Some(PreparedResult::Managed(k_a)) = outer_a_fields.next() else {
        panic!("E's second field must be the managed continuation");
    };
    let PreparedOuterCodegen::Constructor {
        fields: mut node_fields,
        ..
    } = machine
        .inspect_outer(k_a, RealmId::ROOT)
        .expect("k_a's own Node/Leaf FTCQueue cell is ordinary WHNF data and inspects");
    assert_eq!(node_fields.len(), 2, "program's k is Node(Leaf, Leaf)");
    let leaf = take_managed(&mut node_fields, 0);
    let other_leaf = take_managed(&mut node_fields, 1);
    assert!(machine.release(other_leaf));
    let PreparedOuterCodegen::Constructor {
        fields: mut leaf_fields,
        ..
    } = machine
        .inspect_outer(leaf, RealmId::ROOT)
        .expect("Leaf itself is ordinary WHNF data (one field: the closure)");
    assert_eq!(
        leaf_fields.len(),
        1,
        "Leaf has exactly one field: the closure"
    );
    let closure = take_managed(&mut leaf_fields, 0);
    assert!(machine.release(leaf));
    assert!(
        matches!(
            machine.inspect_outer(closure, RealmId::ROOT),
            Err(ExecutionError::Observation(
                ObservationFailure::Unobservable(_)
            ))
        ),
        "a Leaf's own field is a bare closure, never a constructor at WHNF; \
         inspect_outer must refuse it with a typed ObservationFailure"
    );
    assert!(machine.release(closure));
    assert!(machine.release(k_a));

    // A still resumes correctly to completion after B ran unrelated work.
    let value_a = drive_freer_program_to_val(&mut machine, program_a, &fixture, outer_a);
    assert_eq!(value_a, expected_program_value());

    // B's own suspension is untouched by any of the above and still
    // resumes to the same expected value.
    let value_b = drive_freer_program_to_val(&mut machine, program_b, &fixture, outer_b);
    assert_eq!(value_b, expected_program_value());

    assert_eq!(machine.handle_count(), 0);
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}

// ---- S6: end-to-end retained import through the session runtime ---------

const IMPORT_PRODUCER_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-producer.cbor");
const IMPORT_CONSUMER_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-consumer.cbor");
/// `consumerValueAt 0#`'s GHC-computed value, transcribed from
/// `ImportConsumerOracle.hs` run under the pinned GHC 9.12.2. Never
/// hand-derived. (The same file also carries `consumerResult`'s value,
/// read by `s6_direct_global_call_runs_against_the_oracle`.)
const IMPORT_CONSUMER_EXPECTATIONS: &str =
    include_str!("../../haskell/test-prepared-stg/ImportConsumerExpectations.json");

fn tops(prepared: &PreparedProgram) -> Vec<TopBinding> {
    prepared
        .bindings()
        .iter()
        .flat_map(|group| match group {
            tidepool_repr::execution_schema::Group::NonRecursive(top) => {
                std::slice::from_ref(top).to_vec()
            }
            tidepool_repr::execution_schema::Group::Recursive(tops) => tops.clone(),
        })
        .collect()
}

fn top_named(prepared: &PreparedProgram, module: &str, occurrence: &str) -> TopBinding {
    tops(prepared)
        .into_iter()
        .find(|top| top.identity.module == module && top.identity.occurrence == occurrence)
        .unwrap_or_else(|| {
            let available: Vec<String> = tops(prepared)
                .iter()
                .map(|top| {
                    format!(
                        "{}.{} ({:?})",
                        top.identity.module, top.identity.occurrence, top.binding.id
                    )
                })
                .collect();
            panic!("artifact has no top {module}.{occurrence}; tops: {available:?}")
        })
}

/// The first of `occurrences` present as a top of `module`: GHC's
/// worker/wrapper split may leave only the `$w`-prefixed worker as a top.
fn top_named_any(prepared: &PreparedProgram, module: &str, occurrences: &[&str]) -> TopBinding {
    occurrences
        .iter()
        .find_map(|occurrence| {
            tops(prepared)
                .into_iter()
                .find(|top| top.identity.module == module && top.identity.occurrence == *occurrence)
        })
        .unwrap_or_else(|| {
            let available: Vec<String> = tops(prepared)
                .iter()
                .map(|top| format!("{}.{}", top.identity.module, top.identity.occurrence))
                .collect();
            panic!("artifact has no top {module}.{occurrences:?}; tops: {available:?}")
        })
}

/// How many physical arguments a function top's projected signature takes:
/// an unused parameter absence analysis turned into a `Void` rep is declared
/// but not passed.
fn top_arity(prepared: &PreparedProgram, top: &TopBinding) -> usize {
    match &top.binding.rhs {
        tidepool_repr::execution_schema::HeapRhs::Function { signature, .. } => prepared
            .signatures()[signature.0 as usize]
            .arguments
            .iter()
            .filter(|rep| !matches!(rep, tidepool_repr::execution_schema::RuntimeRep::Void))
            .count(),
        _ => 0,
    }
}

/// The [`ImportedValue`] `link_program` checks a declared import against,
/// read from `handle`'s live state under `generation` -- the same facts
/// `PreparedEngine::install` (`tidepool-runtime/src/session/prepared.rs`)
/// assembles per import before linking, and the direct-`PreparedMachine`
/// equivalent of what the deleted `PreparedRuntime::bind_top`'s generation
/// bookkeeping produced.
fn imported_value_for(
    machine: &PreparedMachine<'static>,
    owner: &PreparedProgram,
    top: &TopBinding,
    identity: SymbolIdentity,
    handle: PreparedHandle,
    generation: u64,
) -> ImportedValue {
    let entry_signature = match &top.binding.rhs {
        tidepool_repr::execution_schema::HeapRhs::Function { signature, .. } => {
            Some(owner.signatures()[signature.0 as usize].clone())
        }
        _ => None,
    };
    ImportedValue {
        identity,
        rep: handle.rep(),
        entry_signature,
        evaluated: machine.handle_is_evaluated(handle).expect("handle is live"),
        generation,
    }
}

/// Link and install `prepared` against exactly the imports named -- the
/// direct-`PreparedMachine` equivalent of the deleted
/// `PreparedRuntime::install_prepared`'s link-then-compile-then-install
/// sequence, minus the `SessionVarId`/lease bookkeeping that wrapper alone
/// carried.
fn install_importing(
    machine: &mut PreparedMachine<'static>,
    prepared: PreparedProgram,
    imports: &[(SymbolIdentity, PreparedHandle, ImportedValue)],
) -> Result<ProgramId, PreparedRuntimeError> {
    let mut values = MachineImports::default();
    let mut bindings = ImportBindings::new();
    for (identity, handle, imported) in imports {
        values.values.insert(identity.clone(), imported.clone());
        bindings.insert(identity.clone(), *handle);
    }
    let linked = link_program(prepared, &values)?;
    let compiled = machine
        .compile_for_install(&linked)
        .map_err(PreparedRuntimeError::Compile)?;
    machine
        .install_program(compiled, bindings)
        .map_err(PreparedRuntimeError::Run)
}

fn producer_identity(occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducer".to_owned(),
        namespace: "value".to_owned(),
        occurrence: occurrence.to_owned(),
        record_parent: None,
    }
}

fn expected_consumer_value() -> Vec<i64> {
    let parsed: serde_json::Value = serde_json::from_str(IMPORT_CONSUMER_EXPECTATIONS)
        .expect("ImportConsumerExpectations.json parses as JSON");
    parsed["expectations"]["consumerValueAt"]["value"]
        .as_array()
        .expect("consumerValue expectation is a list")
        .iter()
        .map(|element| {
            element
                .as_i64()
                .expect("consumerValue elements are integers")
        })
        .collect()
}

fn expected_consumer_result() -> i64 {
    let parsed: serde_json::Value = serde_json::from_str(IMPORT_CONSUMER_EXPECTATIONS)
        .expect("ImportConsumerExpectations.json parses as JSON");
    parsed["expectations"]["consumerResult"]["value"]
        .as_i64()
        .expect("consumerResult expectation is an integer")
}

/// An observed boxed `Int`: a bare literal or an `I#` box around one.
fn observed_int(value: &Value) -> i64 {
    match value {
        Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
        Value::Con(_, boxed) => match boxed.as_slice() {
            [Value::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
            other => panic!("unexpected boxed Int {other:?}"),
        },
        other => panic!("unexpected Int shape {other:?}"),
    }
}

/// Flatten an observed `[Int]`: a cons cell is `Con(_, [head, tail])`, nil
/// is `Con(_, [])`, and each head is a bare literal or an `I#` box around one.
fn observed_int_list(value: &Value) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cursor = value;
    loop {
        match cursor {
            Value::Con(_, fields) if fields.is_empty() => return out,
            Value::Con(_, fields) if fields.len() == 2 => {
                let head = match &fields[0] {
                    Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
                    Value::Con(_, boxed) => match boxed.as_slice() {
                        [Value::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
                        other => panic!("unexpected boxed list head {other:?}"),
                    },
                    other => panic!("unexpected list head {other:?}"),
                };
                out.push(head);
                cursor = &fields[1];
            }
            other => panic!("unexpected list shape {other:?}"),
        }
    }
}

/// Rung 2 end to end through the session runtime: install the producer,
/// bind `producerValue` and `producerFn` as retained tops at the generation
/// the consumer was projected against, install the consumer importing both,
/// run its data-only entry with collections between every step and compare
/// against the GHC oracle, then show a consumer linked against a stale
/// generation is refused before anything is installed and that leased
/// bindings cannot be released. `consumerResult` (which applies the
/// imported `producerFn` by calling it directly) is exercised by
/// `s6_direct_global_call_runs_against_the_oracle` below.
#[test]
fn retained_import_end_to_end_links_consumer_against_bound_producer_tops() {
    let producer = parse_program(
        IMPORT_PRODUCER_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("import-producer artifact parses");
    assert!(
        producer.globals().is_empty(),
        "the producer is a closed program"
    );
    let consumer = parse_program(
        IMPORT_CONSUMER_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("import-consumer artifact parses");
    let value_identity = producer_identity("producerValue");
    let fn_identity = producer_identity("producerFn");
    for identity in [&value_identity, &fn_identity] {
        let declaration = consumer
            .globals()
            .iter()
            .find(|global| &global.identity == identity)
            .unwrap_or_else(|| panic!("consumer declares {identity:?} as a global"));
        assert_eq!(
            declaration.required_generation,
            Some(11),
            "S5 pinned both imports at retained generation 11"
        );
    }
    let producer_value_top = top_named(&producer, "ImportProducer", "producerValue");
    let producer_fn_top = top_named(&producer, "ImportProducer", "producerFn");
    let consumer_value_top = top_named_any(
        &consumer,
        "ImportConsumer",
        &["consumerValueAt", "$wconsumerValueAt"],
    );
    let scalar_args = vec![0_u64; top_arity(&consumer, &consumer_value_top)];
    let managed_args: Vec<CodegenPreparedInput> = scalar_args
        .iter()
        .map(|word| CodegenPreparedInput::Scalar(*word))
        .collect();

    let (mut machine, first) = open_closed_machine_from(producer.clone());
    let bound_value = machine
        .retain_top(first, producer_value_top.binding.id)
        .expect("producerValue retains");
    let bound_fn = machine
        .retain_top(first, producer_fn_top.binding.id)
        .expect("producerFn retains");
    assert_eq!(machine.handle_count(), 2);

    let imported_value = imported_value_for(
        &machine,
        &producer,
        &producer_value_top,
        value_identity.clone(),
        bound_value,
        11,
    );
    let imported_fn = imported_value_for(
        &machine,
        &producer,
        &producer_fn_top,
        fn_identity.clone(),
        bound_fn,
        11,
    );
    let program = install_importing(
        &mut machine,
        consumer.clone(),
        &[
            (value_identity.clone(), bound_value, imported_value),
            (fn_identity.clone(), bound_fn, imported_fn),
        ],
    )
    .expect("consumer links against both generation-11 bindings and installs");
    assert_ne!(program, first);
    // `bindings().lease_count(...)` was dead bookkeeping specific to the
    // deleted `PreparedRuntime` wrapper: `PreparedEngine::install` (the
    // production owner) never calls `BindingTable::acquire_leases`, so no
    // import is ever leased on the real prepared route either. Dropped.

    let expected = expected_consumer_value();
    let observed = machine
        .run_entry(
            program,
            consumer_value_top.binding.id,
            &scalar_args,
            call_options(true),
            RealmId::ROOT,
        )
        .expect("consumerValueAt reads producerValue through its import slot");
    assert_eq!(observed.values.len(), 1);
    assert_eq!(observed_int_list(&observed.values[0]), expected);
    let again = machine
        .run_entry(
            program,
            consumer_value_top.binding.id,
            &scalar_args,
            call_options(true),
            RealmId::ROOT,
        )
        .expect("a second run after another collection reads the same import");
    assert_eq!(observed_int_list(&again.values[0]), expected);

    // The consumer reads through the slot: one new root for the retained
    // result, the two bound roots untouched, and the result releases.
    let mut retained = machine
        .run_entry_retained(
            program,
            consumer_value_top.binding.id,
            &managed_args,
            call_options(true),
            RealmId::ROOT,
        )
        .expect("consumerValueAt retains");
    assert_eq!(machine.handle_count(), 3);
    let value = take_managed(&mut retained.values, 0);
    assert!(machine.release(value));
    assert_eq!(machine.handle_count(), 2);

    // The pinned target holds the imported function as a constructor field:
    // it flows through bind/link/install as a value and is readable.
    let entries_top = top_named_any(
        &consumer,
        "ImportConsumer",
        &["consumerEntries", "$wconsumerEntries"],
    );
    let entries_args: Vec<CodegenPreparedInput> = (0..top_arity(&consumer, &entries_top))
        .map(|_| CodegenPreparedInput::Scalar(0))
        .collect();
    let mut entries = machine
        .run_entry_retained(
            program,
            entries_top.binding.id,
            &entries_args,
            call_options(true),
            RealmId::ROOT,
        )
        .expect("consumerEntries builds its pair at run time");
    let pair = take_managed(&mut entries.values, 0);
    let PreparedOuterCodegen::Constructor {
        fields: mut pair_fields,
        ..
    } = machine
        .inspect_outer(pair, RealmId::ROOT)
        .expect("the pair inspects");
    assert!(machine.release(pair));
    assert_eq!(pair_fields.len(), 2);
    let list = take_managed(&mut pair_fields, 0);
    let function = take_managed(&mut pair_fields, 1);
    // The list field is this module's own lazy `consumerValueAt n`: a thunk
    // the host never forces (the evaluated list was already read above
    // through the entry itself).
    match machine.inspect_outer(list, RealmId::ROOT) {
        Err(ExecutionError::Observation(ObservationFailure::Unobservable(kind))) => {
            assert_eq!(format!("{kind:?}"), "Thunk")
        }
        Err(other) => panic!("expected the typed Unobservable(Thunk) refusal, got {other:?}"),
        Ok(_) => panic!("an unforced thunk must not inspect as a constructor"),
    }
    assert!(machine.release(list));
    match machine.inspect_outer(function, RealmId::ROOT) {
        Err(ExecutionError::Observation(ObservationFailure::Unobservable(kind))) => assert_eq!(
            format!("{kind:?}"),
            "Function",
            "the imported function is held as a callable, never entered"
        ),
        Err(other) => panic!("expected the typed Unobservable(Function) refusal, got {other:?}"),
        Ok(_) => panic!("a function-typed import must not inspect as a constructor"),
    }
    assert!(machine.release(function));
    assert_eq!(machine.handle_count(), 2);

    // A consumer projected against generation 11 does not link against
    // bindings whose `ImportedValue.generation` reads 12, and the refusal
    // installs nothing. `PreparedMachine` carries no generation-stamped
    // binding table of its own (that tracking lived only in the deleted
    // `PreparedRuntime` wrapper's `bind_top`/`set_val_gen`), so this step
    // retains two fresh handles to the same tops and hand-builds their
    // `ImportedValue`s at a fabricated stale generation instead of
    // advancing a real session generation counter and rebinding.
    let stale_value = machine
        .retain_top(first, producer_value_top.binding.id)
        .expect("producerValue retains again for the stale-generation probe");
    let stale_fn = machine
        .retain_top(first, producer_fn_top.binding.id)
        .expect("producerFn retains again for the stale-generation probe");
    let handles_before = machine.handle_count();
    let stale_value_import = imported_value_for(
        &machine,
        &producer,
        &producer_value_top,
        value_identity.clone(),
        stale_value,
        12,
    );
    let stale_fn_import = imported_value_for(
        &machine,
        &producer,
        &producer_fn_top,
        fn_identity.clone(),
        stale_fn,
        12,
    );
    let error = install_importing(
        &mut machine,
        consumer,
        &[
            (value_identity.clone(), stale_value, stale_value_import),
            (fn_identity.clone(), stale_fn, stale_fn_import),
        ],
    )
    .expect_err("a stale generation must not link");
    assert!(
        matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
        "expected ImportContract, got {error:?}"
    );
    assert_eq!(error.kind(), PreparedFailureKind::Rejected);
    assert_eq!(machine.handle_count(), handles_before);
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // `bound_value`/`bound_fn` are left retained (the production
    // `PreparedEngine` route never leases them either, so nothing refuses
    // their release the way the deleted `PreparedRuntime` wrapper's
    // `BindingLeased` did; leaving them alive here matches the same
    // "installed program's import slot stays live" end state). The
    // never-installed-against stale handles release cleanly.
    assert!(machine.release(stale_value));
    assert!(machine.release(stale_fn));
    assert_eq!(machine.handle_count(), 2);
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
}

const IMPORT_CONSUMER_RESULT_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-consumer-result.cbor");

/// S6, the direct-call half: `consumerResultAt 0#` -- whose body is
/// `producerFn (length producerValue)`, the imported function called
/// directly by name (`ValueRef::Global` callee) -- installs, links against
/// the two generation-11 bindings, runs through the machine-wide resolver,
/// and returns the GHC oracle's value. Before G0 this exact artifact was
/// refused at admission (a closed-world `Call` arm with no `Global` case),
/// which is why it is pinned as its own fixture rather than bundled into
/// `import-consumer.cbor`.
#[test]
fn s6_direct_global_call_runs_against_the_oracle() {
    let producer = parse_program(
        IMPORT_PRODUCER_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("import-producer artifact parses");
    let consumer_result = parse_program(
        IMPORT_CONSUMER_RESULT_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("import-consumer-result artifact parses");
    let value_identity = producer_identity("producerValue");
    let fn_identity = producer_identity("producerFn");
    for identity in [&value_identity, &fn_identity] {
        let declaration = consumer_result
            .globals()
            .iter()
            .find(|global| &global.identity == identity)
            .unwrap_or_else(|| panic!("consumer-result declares {identity:?} as a global"));
        assert_eq!(
            declaration.required_generation,
            Some(11),
            "projected against the same retained generation as the pinned consumer"
        );
    }
    let producer_value_top = top_named(&producer, "ImportProducer", "producerValue");
    let producer_fn_top = top_named(&producer, "ImportProducer", "producerFn");

    let (mut machine, first) = open_closed_machine_from(producer.clone());
    let bound_value = machine
        .retain_top(first, producer_value_top.binding.id)
        .expect("producerValue retains");
    let bound_fn = machine
        .retain_top(first, producer_fn_top.binding.id)
        .expect("producerFn retains");

    let result_top = top_named_any(
        &consumer_result,
        "ImportConsumer",
        &["consumerResultAt", "$wconsumerResultAt"],
    );
    let scalar_args = vec![0_u64; top_arity(&consumer_result, &result_top)];

    let imported_value = imported_value_for(
        &machine,
        &producer,
        &producer_value_top,
        value_identity.clone(),
        bound_value,
        11,
    );
    let imported_fn = imported_value_for(
        &machine,
        &producer,
        &producer_fn_top,
        fn_identity.clone(),
        bound_fn,
        11,
    );
    let program = install_importing(
        &mut machine,
        consumer_result,
        &[
            (value_identity.clone(), bound_value, imported_value),
            (fn_identity.clone(), bound_fn, imported_fn),
        ],
    )
    .expect("a direct call to an imported function is admitted and links");
    assert_ne!(program, first);
    // `bindings().lease_count(...)` was dead bookkeeping specific to the
    // deleted `PreparedRuntime` wrapper -- see the matching comment in
    // `retained_import_end_to_end_links_consumer_against_bound_producer_tops`.
    // Dropped.

    let observed = machine
        .run_entry(
            program,
            result_top.binding.id,
            &scalar_args,
            call_options(true),
            RealmId::ROOT,
        )
        .expect("consumerResultAt applies the imported producerFn through the resolver");
    assert_eq!(observed.values.len(), 1);
    assert_eq!(
        observed_int(&observed.values[0]),
        expected_consumer_result(),
        "producerFn (length producerValue) per ImportConsumerOracle.hs"
    );
    let again = machine
        .run_entry(
            program,
            result_top.binding.id,
            &scalar_args,
            call_options(true),
            RealmId::ROOT,
        )
        .expect("a second run after another collection resolves the same import");
    assert_eq!(observed_int(&again.values[0]), expected_consumer_result());

    assert_eq!(machine.handle_count(), 2);
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    // The deleted `PreparedRuntime` wrapper's `release_binding(bound_fn)`
    // refused with `BindingLeased` here because ITS OWN bookkeeping leased
    // every import; `PreparedMachine` (and the production `PreparedEngine`
    // route) never leases, so `bound_fn` is simply left retained -- the
    // same "installed program's import slot stays live" state, reached
    // without a lease-refusal assertion to port.
}
