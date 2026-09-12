//! Independently observed GHC, reference-evaluator, and JIT baselines.
//!
//! Each evaluator runs to a typed outcome before this test compares anything.
//! Reference and JIT execution happen in owned child test processes so a hang
//! or native fault cannot take out the observer or be mistaken for agreement.

use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tidepool_codegen::emit::EmitError;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::machine_state::MachineDisposition;
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, EvalError, Value, VecHeap};
use tidepool_repr::Literal;
use tidepool_testing::eval_harness::{require_extract, with_eval_stack, CompileError, EvalHarness};

use crate::engine_observation::{EngineOutcome, SupportCategory, SupportItem, SupportStatus};

const SOURCE: &str = include_str!("fixtures/EngineReview.hs");
const CHILD_ENGINE_ENV: &str = "TIDEPOOL_ENGINE_REVIEW_CHILD_ENGINE";
const CHILD_TARGET_ENV: &str = "TIDEPOOL_ENGINE_REVIEW_CHILD_TARGET";
const CHILD_RESULT_PREFIX: &str = "TIDEPOOL_ENGINE_REVIEW_RESULT=";
const CHILD_COMPILE_ERROR_EXIT: i32 = 20;
const CHILD_RUNTIME_ERROR_EXIT: i32 = 21;
const CHILD_CANCELLED_EXIT: i32 = 22;
const CHILD_INTEGRITY_ERROR_EXIT: i32 = 23;
const CHILD_UNSUPPORTED_EXIT: i32 = 24;
const CHILD_COMPILER_ERROR_EXIT: i32 = 25;
const CHILD_OPERATIONAL_ERROR_EXIT: i32 = 26;
const ENGINE_DEADLINE: Duration = Duration::from_secs(60);
const GHC_DEADLINE: Duration = Duration::from_secs(30);

type BaselineOutcome = EngineOutcome<i64, String, String, String>;

#[derive(Debug)]
struct Observations {
    native: BaselineOutcome,
    reference: BaselineOutcome,
    jit: BaselineOutcome,
}

/// M0 inventory only: M1's real prepared output establishes the exhaustive
/// names and the M3 schema. Every row deliberately remains unverified here.
const PROVISIONAL_SUPPORT: &[SupportItem] = &[
    SupportItem {
        category: SupportCategory::Expression,
        name: "application / literal / constructor / primitive",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 projection and differential cases",
    },
    SupportItem {
        category: SupportCategory::Expression,
        name: "case / let / let-no-escape / tick",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 projection and differential cases",
    },
    SupportItem {
        category: SupportCategory::RightHandSide,
        name: "closure and constructor RHS",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 closure and layout facts",
    },
    SupportItem {
        category: SupportCategory::Binding,
        name: "non-recursive and recursive groups",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 dependency groups",
    },
    SupportItem {
        category: SupportCategory::Alternative,
        name: "default, literal and constructor alternatives",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 alternative binders",
    },
    SupportItem {
        category: SupportCategory::UpdatePolicy,
        name: "ReEntrant / Updatable / SingleEntry / JumpedTo",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 update flags and M3 semantics",
    },
    SupportItem {
        category: SupportCategory::RuntimeRepresentation,
        name: "lifted references, unlifted references, scalar and zero-width components",
        status: SupportStatus::Unverified,
        evidence: "needs real M1 unarised signatures",
    },
    SupportItem {
        category: SupportCategory::Primitive,
        name: "pinned GHC primitive and foreign intrinsic inventory",
        status: SupportStatus::Unverified,
        evidence: "requires exhaustive M1 inventory and named boundary tests",
    },
    SupportItem {
        category: SupportCategory::ResidentValue,
        name: "retained imports and cross-fragment calls",
        status: SupportStatus::Unverified,
        evidence: "requires actual resident import contract",
    },
    SupportItem {
        category: SupportCategory::Effect,
        name: "typed sites, suspension, resumption and sibling continuations",
        status: SupportStatus::Unverified,
        evidence: "requires production effect-route coverage",
    },
];

#[test]
fn m0_outcome_vocabulary_does_not_collapse_failures() {
    let failures: [BaselineOutcome; 10] = [
        EngineOutcome::LanguageRejected("language".into()),
        EngineOutcome::CompilerFailure("compiler".into()),
        EngineOutcome::RuntimeError("runtime".into()),
        EngineOutcome::Cancelled("cancelled".into()),
        EngineOutcome::IntegrityFailure("integrity".into()),
        EngineOutcome::OperationalFailure("operational".into()),
        EngineOutcome::UnsupportedInput("unsupported".into()),
        EngineOutcome::Timeout,
        EngineOutcome::NativeFault { signal: 11 },
        EngineOutcome::HarnessError("observer".into()),
    ];
    for (left_index, left) in failures.iter().enumerate() {
        for (right_index, right) in failures.iter().enumerate() {
            assert_eq!(left == right, left_index == right_index);
        }
    }
    assert_eq!(
        EngineOutcome::<u8, String, String, String>::Success(3).map_success(u16::from),
        EngineOutcome::Success(3)
    );
}

#[test]
fn m0_support_inventory_keeps_every_provisional_row_unverified() {
    let future_states = [SupportStatus::Verified, SupportStatus::Unsupported];
    assert_ne!(future_states[0], future_states[1]);
    assert!(!PROVISIONAL_SUPPORT.is_empty());
    assert!(PROVISIONAL_SUPPORT.iter().all(|item| {
        item.status == SupportStatus::Unverified
            && !item.name.is_empty()
            && !item.evidence.is_empty()
    }));
    for category in [
        SupportCategory::Expression,
        SupportCategory::RightHandSide,
        SupportCategory::Binding,
        SupportCategory::Alternative,
        SupportCategory::UpdatePolicy,
        SupportCategory::RuntimeRepresentation,
        SupportCategory::Primitive,
        SupportCategory::ResidentValue,
        SupportCategory::Effect,
    ] {
        assert!(PROVISIONAL_SUPPORT
            .iter()
            .any(|item| item.category == category));
    }
}

#[derive(Debug, Clone, Copy)]
enum ChildEngine {
    Reference,
    Jit,
}

impl ChildEngine {
    fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Jit => "jit",
        }
    }
}

enum BoundedOutput {
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Timeout,
    HarnessError(String),
}

/// Spawn, bound, kill, and reap exactly one owned child. Reader threads keep a
/// verbose compiler failure from filling a pipe and deadlocking the observer.
fn run_bounded(command: &mut Command, deadline: Duration) -> BoundedOutput {
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return BoundedOutput::HarnessError(error.to_string()),
    };
    let mut stdout = child.stdout.take().expect("piped child stdout");
    let mut stderr = child.stderr.take().expect("piped child stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let expires = Instant::now() + deadline;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < expires => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return BoundedOutput::HarnessError(error.to_string());
            }
        }
    };
    let stdout = match stdout_reader.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return BoundedOutput::HarnessError(error.to_string()),
        Err(_) => return BoundedOutput::HarnessError("stdout reader panicked".into()),
    };
    let stderr = match stderr_reader.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return BoundedOutput::HarnessError(error.to_string()),
        Err(_) => return BoundedOutput::HarnessError("stderr reader panicked".into()),
    };
    match status {
        Some(status) => BoundedOutput::Exited {
            status,
            stdout,
            stderr,
        },
        None => BoundedOutput::Timeout,
    }
}

fn native_outcome(target: &str) -> BaselineOutcome {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/EngineReview.hs");
    let mut compile = Command::new("ghc");
    compile
        .arg("-ignore-dot-ghci")
        .args(["-fno-code", "-fforce-recomp"])
        .arg(&fixture);
    match run_bounded(&mut compile, GHC_DEADLINE) {
        BoundedOutput::Timeout => return EngineOutcome::Timeout,
        BoundedOutput::HarnessError(error) => return EngineOutcome::HarnessError(error),
        BoundedOutput::Exited { status, stderr, .. } if !status.success() => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return EngineOutcome::NativeFault { signal };
                }
            }
            return EngineOutcome::CompilerFailure(String::from_utf8_lossy(&stderr).into_owned());
        }
        BoundedOutput::Exited { .. } => {}
    }

    let mut execute = Command::new("ghc");
    execute
        .arg("-ignore-dot-ghci")
        .arg(fixture)
        .args(["-e", &format!("print EngineReview.{target}")]);
    match run_bounded(&mut execute, GHC_DEADLINE) {
        BoundedOutput::Timeout => EngineOutcome::Timeout,
        BoundedOutput::HarnessError(error) => EngineOutcome::HarnessError(error),
        BoundedOutput::Exited {
            status,
            stdout,
            stderr,
        } => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return EngineOutcome::NativeFault { signal };
                }
            }
            if !status.success() {
                return EngineOutcome::RuntimeError(String::from_utf8_lossy(&stderr).into_owned());
            }
            match String::from_utf8(stdout)
                .ok()
                .and_then(|stdout| stdout.trim().parse().ok())
            {
                Some(value) => EngineOutcome::Success(value),
                None => EngineOutcome::HarnessError("GHC produced a non-integer answer".into()),
            }
        }
    }
}

fn child_test_name() -> String {
    let module = module_path!()
        .split_once("::")
        .expect("integration test module")
        .1;
    format!("{module}::m0_engine_observer_child")
}

fn isolated_outcome(engine: ChildEngine, target: &str) -> BaselineOutcome {
    isolated_outcome_named(engine.name(), target, ENGINE_DEADLINE)
}

fn isolated_outcome_named(engine: &str, target: &str, deadline: Duration) -> BaselineOutcome {
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return EngineOutcome::HarnessError(error.to_string()),
    };
    let mut command = Command::new(executable);
    command
        .args([
            "--exact",
            &child_test_name(),
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENGINE_ENV, engine)
        .env(CHILD_TARGET_ENV, target);
    match run_bounded(&mut command, deadline) {
        BoundedOutput::Timeout => EngineOutcome::Timeout,
        BoundedOutput::HarnessError(error) => EngineOutcome::HarnessError(error),
        BoundedOutput::Exited {
            status,
            stdout,
            stderr,
        } => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return EngineOutcome::NativeFault { signal };
                }
            }
            let diagnostics = || {
                format!(
                    "child status {status}; stdout: {}; stderr: {}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                )
            };
            match status.code() {
                Some(CHILD_COMPILE_ERROR_EXIT) => {
                    return EngineOutcome::LanguageRejected(
                        String::from_utf8_lossy(&stderr).into_owned(),
                    );
                }
                Some(CHILD_RUNTIME_ERROR_EXIT) => {
                    return EngineOutcome::RuntimeError(
                        String::from_utf8_lossy(&stderr).into_owned(),
                    );
                }
                Some(CHILD_CANCELLED_EXIT) => {
                    return EngineOutcome::Cancelled(String::from_utf8_lossy(&stderr).into_owned());
                }
                Some(CHILD_INTEGRITY_ERROR_EXIT) => {
                    return EngineOutcome::IntegrityFailure(
                        String::from_utf8_lossy(&stderr).into_owned(),
                    );
                }
                Some(CHILD_UNSUPPORTED_EXIT) => {
                    return EngineOutcome::UnsupportedInput(
                        String::from_utf8_lossy(&stderr).into_owned(),
                    );
                }
                Some(CHILD_COMPILER_ERROR_EXIT) => {
                    return EngineOutcome::CompilerFailure(
                        String::from_utf8_lossy(&stderr).into_owned(),
                    );
                }
                Some(CHILD_OPERATIONAL_ERROR_EXIT) => {
                    return EngineOutcome::OperationalFailure(
                        String::from_utf8_lossy(&stderr).into_owned(),
                    );
                }
                _ => {}
            }
            if !status.success() {
                return EngineOutcome::HarnessError(diagnostics());
            }
            let stdout_text = String::from_utf8_lossy(&stdout);
            let marker = stdout_text
                .split_once(CHILD_RESULT_PREFIX)
                .and_then(|(_, suffix)| suffix.split_whitespace().next());
            match marker.and_then(|value| value.parse().ok()) {
                Some(value) => EngineOutcome::Success(value),
                None => EngineOutcome::HarnessError(diagnostics()),
            }
        }
    }
}

fn observe(target: &str) -> Observations {
    Observations {
        native: native_outcome(target),
        reference: isolated_outcome(ChildEngine::Reference, target),
        jit: isolated_outcome(ChildEngine::Jit, target),
    }
}

fn int_value(value: &Value) -> Result<i64, String> {
    match value {
        Value::Lit(Literal::LitInt(n)) => Ok(*n),
        Value::Con(_, fields) if fields.len() == 1 => int_value(&fields[0]),
        other => Err(format!("expected Int, got {other:?}")),
    }
}

fn child_outcome(engine: ChildEngine, target: &str) -> BaselineOutcome {
    let compiled = match EvalHarness::new().compile(SOURCE, target) {
        Ok(compiled) => compiled,
        Err(error) => return classify_compile_error(error),
    };
    with_eval_stack(move || match engine {
        ChildEngine::Reference => {
            let mut heap = VecHeap::new();
            match eval(
                &compiled.expr,
                &env_from_datacon_table(&compiled.table),
                &mut heap,
            )
            .and_then(|value| deep_force(value, &mut heap))
            {
                Ok(value) => int_value(&value)
                    .map(EngineOutcome::Success)
                    .unwrap_or_else(EngineOutcome::HarnessError),
                Err(error) => classify_eval_error(error),
            }
        }
        ChildEngine::Jit => {
            let mut machine =
                match JitEffectMachine::compile(&compiled.expr, &compiled.table, 1024 * 1024) {
                    Ok(machine) => machine,
                    Err(error) => return classify_jit_error(error),
                };
            match machine.run_pure() {
                Ok(value) => int_value(&value)
                    .map(EngineOutcome::Success)
                    .unwrap_or_else(EngineOutcome::HarnessError),
                Err(error) => classify_jit_error(error),
            }
        }
    })
}

fn classify_compile_error(error: CompileError) -> BaselineOutcome {
    let detail = format!("{error:?}");
    match error {
        CompileError::Diagnostics(_) => EngineOutcome::LanguageRejected(detail),
        CompileError::IOTypeDetected => EngineOutcome::UnsupportedInput(detail),
        _ => EngineOutcome::CompilerFailure(detail),
    }
}

fn classify_eval_error(error: EvalError) -> BaselineOutcome {
    let detail = format!("{error:?}");
    match error {
        EvalError::UnsupportedPrimOp(_) => EngineOutcome::UnsupportedInput(detail),
        EvalError::UnboundVar(_)
        | EvalError::UnboundJoin(_)
        | EvalError::InternalError(_)
        | EvalError::JumpInFlight => EngineOutcome::OperationalFailure(detail),
        _ => EngineOutcome::RuntimeError(detail),
    }
}

fn classify_jit_error(error: JitError) -> BaselineOutcome {
    let detail = format!("{error:?}");
    match error {
        JitError::Compilation(EmitError::NotYetImplemented(_)) => {
            EngineOutcome::UnsupportedInput(detail)
        }
        JitError::Yield(YieldError::Runtime(RuntimeError::Cancelled)) => {
            EngineOutcome::Cancelled(detail)
        }
        JitError::Yield(YieldError::Runtime(runtime))
            if runtime.machine_disposition() == MachineDisposition::Unavailable =>
        {
            EngineOutcome::IntegrityFailure(detail)
        }
        JitError::Yield(YieldError::Runtime(_)) => EngineOutcome::RuntimeError(detail),
        JitError::Yield(YieldError::Signal(_)) | JitError::Signal(_) => {
            EngineOutcome::OperationalFailure(detail)
        }
        JitError::MachineUnavailable { .. } => EngineOutcome::IntegrityFailure(detail),
        JitError::Compilation(_) | JitError::Pipeline(_) | JitError::MissingConTags(_) => {
            EngineOutcome::CompilerFailure(detail)
        }
        _ => EngineOutcome::OperationalFailure(detail),
    }
}

#[test]
fn failure_classification_uses_owning_types() {
    assert!(matches!(
        classify_compile_error(CompileError::WorkerFailure(Vec::new())),
        EngineOutcome::CompilerFailure(_)
    ));
    assert!(matches!(
        classify_compile_error(CompileError::IOTypeDetected),
        EngineOutcome::UnsupportedInput(_)
    ));
    assert!(matches!(
        classify_jit_error(RuntimeError::DivisionByZero.into()),
        EngineOutcome::RuntimeError(_)
    ));
    assert!(matches!(
        classify_jit_error(RuntimeError::Cancelled.into()),
        EngineOutcome::Cancelled(_)
    ));
    assert!(matches!(
        classify_jit_error(RuntimeError::UnresolvedVar(7, None).into()),
        EngineOutcome::IntegrityFailure(_)
    ));
    assert!(matches!(
        classify_jit_error(JitError::Compilation(EmitError::NotYetImplemented(
            "M0 unsupported-input probe".into()
        ))),
        EngineOutcome::UnsupportedInput(_)
    ));
    assert!(matches!(
        classify_jit_error(JitError::InvalidSuspensionState("M0 operational probe")),
        EngineOutcome::OperationalFailure(_)
    ));
}

/// Private subprocess entry selected only by `isolated_outcome`. It goes
/// through `EvalHarness`, so extraction remains owned by the existing test
/// launcher and inherited toolchain environment.
#[test]
#[ignore = "subprocess entry for the M0 observer"]
fn m0_engine_observer_child() {
    let Ok(engine) = std::env::var(CHILD_ENGINE_ENV) else {
        return;
    };
    let target = std::env::var(CHILD_TARGET_ENV).expect("child target");
    let engine = match engine.as_str() {
        "reference" => ChildEngine::Reference,
        "jit" => ChildEngine::Jit,
        "language" => emit_child_outcome(classify_compile_error(CompileError::Diagnostics(
            Vec::new(),
        ))),
        "cancelled" => emit_child_outcome(classify_jit_error(RuntimeError::Cancelled.into())),
        "integrity" => emit_child_outcome(classify_jit_error(
            RuntimeError::UnresolvedVar(7, None).into(),
        )),
        "unsupported" => emit_child_outcome(classify_jit_error(JitError::Compilation(
            EmitError::NotYetImplemented("M0 unsupported-input probe".into()),
        ))),
        "compiler" => emit_child_outcome(classify_compile_error(CompileError::WorkerFailure(
            Vec::new(),
        ))),
        "operational" => emit_child_outcome(classify_jit_error(JitError::InvalidSuspensionState(
            "M0 operational probe",
        ))),
        "hang" => {
            std::thread::sleep(Duration::from_secs(10));
            return;
        }
        #[cfg(unix)]
        "fault" => {
            // This child is disposable and must not leave a large core file.
            unsafe {
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                assert_eq!(libc::setrlimit(libc::RLIMIT_CORE, &limit), 0);
                libc::raise(libc::SIGABRT);
            }
            unreachable!("SIGABRT returned");
        }
        "harness" => panic!("deliberate observer harness failure"),
        other => panic!("unknown child engine {other:?}"),
    };
    emit_child_outcome(child_outcome(engine, &target))
}

fn emit_child_outcome(outcome: BaselineOutcome) -> ! {
    match outcome {
        EngineOutcome::Success(value) => {
            println!("{CHILD_RESULT_PREFIX}{value}");
            std::io::stdout().flush().expect("flush child result");
        }
        EngineOutcome::LanguageRejected(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_COMPILE_ERROR_EXIT);
        }
        EngineOutcome::CompilerFailure(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_COMPILER_ERROR_EXIT);
        }
        EngineOutcome::RuntimeError(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_RUNTIME_ERROR_EXIT);
        }
        EngineOutcome::Cancelled(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_CANCELLED_EXIT);
        }
        EngineOutcome::IntegrityFailure(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_INTEGRITY_ERROR_EXIT);
        }
        EngineOutcome::OperationalFailure(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_OPERATIONAL_ERROR_EXIT);
        }
        EngineOutcome::UnsupportedInput(error) => {
            eprintln!("{error}");
            std::io::stderr().flush().expect("flush child diagnostic");
            std::process::exit(CHILD_UNSUPPORTED_EXIT);
        }
        EngineOutcome::HarnessError(error) => panic!("child harness failure: {error}"),
        EngineOutcome::Timeout | EngineOutcome::NativeFault { .. } => {
            unreachable!("the parent process classifies process termination")
        }
    }
    std::process::exit(0)
}

#[cfg(unix)]
#[test]
fn process_bound_preserves_awkward_failure_classifications() {
    require_extract();
    assert!(matches!(
        isolated_outcome_named("language", "unused", Duration::from_secs(5)),
        EngineOutcome::LanguageRejected(_)
    ));
    assert!(matches!(
        isolated_outcome_named("reference", "missingEngineReviewTarget", ENGINE_DEADLINE),
        EngineOutcome::CompilerFailure(_)
    ));
    assert!(matches!(
        isolated_outcome_named("cancelled", "unused", Duration::from_secs(5)),
        EngineOutcome::Cancelled(_)
    ));
    assert!(matches!(
        isolated_outcome_named("integrity", "unused", Duration::from_secs(5)),
        EngineOutcome::IntegrityFailure(_)
    ));
    assert!(matches!(
        isolated_outcome_named("unsupported", "unused", Duration::from_secs(5)),
        EngineOutcome::UnsupportedInput(_)
    ));
    assert!(matches!(
        isolated_outcome_named("compiler", "unused", Duration::from_secs(5)),
        EngineOutcome::CompilerFailure(_)
    ));
    assert!(matches!(
        isolated_outcome_named("operational", "unused", Duration::from_secs(5)),
        EngineOutcome::OperationalFailure(_)
    ));
    assert_eq!(
        isolated_outcome_named("hang", "unused", Duration::from_millis(50)),
        EngineOutcome::Timeout
    );
    assert!(matches!(
        isolated_outcome_named("fault", "unused", Duration::from_secs(5)),
        EngineOutcome::NativeFault {
            signal: libc::SIGABRT
        }
    ));
    assert!(matches!(
        isolated_outcome_named("harness", "unused", Duration::from_secs(5)),
        EngineOutcome::HarnessError(_)
    ));
}

fn assert_successful_agreement(target: &str) {
    require_extract();
    let observations = observe(target);
    match (
        &observations.native,
        &observations.reference,
        &observations.jit,
    ) {
        (
            EngineOutcome::Success(native),
            EngineOutcome::Success(reference),
            EngineOutcome::Success(jit),
        ) => {
            assert!(
                reference == native && jit == native,
                "{target} disagreed after three independent successes: {observations:#?}"
            );
        }
        _ => panic!("{target} did not succeed independently: {observations:#?}"),
    }
}

fn assert_independent_runtime_errors(target: &str) {
    require_extract();
    let observations = observe(target);
    assert!(
        matches!(observations.native, EngineOutcome::RuntimeError(_))
            && matches!(observations.reference, EngineOutcome::RuntimeError(_))
            && matches!(observations.jit, EngineOutcome::RuntimeError(_)),
        "{target} did not report three independent runtime errors: {observations:#?}"
    );
}

#[test]
#[ignore = "required prepared-STG acceptance: M0 outcome is recorded in engine-m0-baseline.md"]
fn user_defined_append_matches_ghc() {
    assert_successful_agreement("customAppend");
}

#[test]
#[ignore = "required prepared-STG acceptance: M0 outcome is recorded in engine-m0-baseline.md"]
fn nul_string_matches_ghc() {
    assert_successful_agreement("nulString");
}

#[test]
#[ignore = "required prepared-STG acceptance: M0 outcome is recorded in engine-m0-baseline.md"]
fn reference_evaluator_preserves_lazy_arguments() {
    assert_successful_agreement("lazyArgument");
}

#[test]
#[ignore = "required prepared-STG acceptance: M0 outcome is recorded in engine-m0-baseline.md"]
fn jit_does_not_enter_unused_recursive_argument() {
    assert_successful_agreement("unusedLoop");
}

#[test]
fn lazy_constructor_field_matches_ghc() {
    assert_successful_agreement("lazyConstructor");
}

#[test]
fn returned_function_application_matches_ghc() {
    assert_successful_agreement("returnedFunction");
}

#[test]
fn multi_parameter_tail_recursion_matches_ghc() {
    assert_successful_agreement("multiParameterRecursion");
}

#[test]
fn multibyte_chars_match_ghc() {
    assert_successful_agreement("multibyteChars");
}

#[test]
fn strict_field_failure_is_recorded_per_engine() {
    assert_independent_runtime_errors("strictFieldFailure");
}
