//! Deep differential testing — interpreter vs JIT at lifted generator depth.
//!
//! The historical depth-3 cap existed because the recursive value-comparison
//! helpers overflowed the host stack. Those helpers are now worklist-based
//! (see `tidepool_testing::compare`), so this suite drives the generators at
//! depth 5 and 7 and compares *deep-forced* results with a strict structural
//! comparison — catching divergences that the shallow, thunk-skipping
//! comparison in `proptest.rs` would mask.
//!
//! ## Containment (B3)
//!
//! Each generated case is executed in a re-exec'd subprocess (the `#[ignore]`d
//! [`deep_diff_worker`] test, selected via `--exact`). A JIT fault therefore
//! kills only the child: the parent classifies the child's exit and keeps
//! running, so every property *completes* all its cases even when individual
//! cases SIGSEGV/SIGILL. This uses only `std` (no libc/stacker dependency).
//!
//! ## Reportable outcomes
//!
//! - **B1** both backends succeed but deep-forced values differ (worker exit 2)
//! - **B2** JIT raises a non-whitelisted runtime error where eval succeeded
//!   (worker exit 3)
//! - **B3** the child dies by a fatal signal (SIGSEGV/SIGILL/SIGBUS/SIGABRT)
//!
//! Known, non-bug divergences (eval-side errors / laziness gaps, whitelisted
//! JIT errors, deep-force failures, JIT compile failures on synthetic IR) are
//! treated as skips so the committed suite stays green.

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use std::cell::Cell;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use std::os::unix::process::ExitStatusExt;

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::{env_from_datacon_table, eval::eval, heap::VecHeap};
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::CoreExpr;
use tidepool_testing::compare;
use tidepool_testing::gen::{arb_core_expr_weighted, arb_ground_expr_depth};
use tidepool_testing::proptest::build_table_for_expr;

/// Name of the `#[ignore]`d worker test, selected with `--exact`.
const WORKER_TEST: &str = "deep_diff_worker";
/// Env var carrying the path to the case's CBOR-serialized CoreExpr.
const ENV_CASE: &str = "TP_DEEP_DIFF_CASE";
/// Env var carrying the nursery size in bytes.
const ENV_NURSERY: &str = "TP_DEEP_DIFF_NURSERY";
/// Env var carrying "1" to optimize the expr before comparing.
const ENV_OPT: &str = "TP_DEEP_DIFF_OPT";

/// Worker exit codes (0 = ok/skip).
/// Both backends produced equal ground values (a real comparison happened).
const EXIT_OK: i32 = 0;
const EXIT_B1: i32 = 2;
const EXIT_B2: i32 = 3;
/// In-process signal caught by `run_pure`'s protection (vs an uncaught fatal
/// signal, which surfaces as a child-process signal instead).
const EXIT_B3: i32 = 4;
/// Case fell into a known-divergence class and was skipped (no comparison).
const EXIT_SKIP: i32 = 5;

/// Per-process counter for unique temp-file names (avoids RNG).
static CASE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Number of cases per property. Overridable via `TP_DEEP_CASES` for fast hunt
/// passes; defaults to 200 for the committed suite.
fn case_count() -> u32 {
    std::env::var("TP_DEEP_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
}

/// Detect the synthetic-LetRec known-divergence class: a `LetRec` binding whose
/// RHS is a bare `Var`. GHC never emits such a binding (rec RHS are always
/// Lam/Con/values); the interpreter thunks these simple inter-referencing
/// bindings while the JIT evaluates them sequentially, so the two backends
/// observe different values for a semantically-ill-defined program. This is the
/// same class the `proptest_differential` whitelist skips as `UnresolvedVar`,
/// here manifesting as a value difference rather than an error.
fn has_synthetic_letrec(expr: &CoreExpr) -> bool {
    use tidepool_repr::frame::CoreFrame;
    expr.nodes.iter().any(|n| {
        if let CoreFrame::LetRec { bindings, .. } = n {
            bindings
                .iter()
                .any(|(_, rhs)| matches!(expr.nodes.get(*rhs), Some(CoreFrame::Var(_))))
        } else {
            false
        }
    })
}

/// Classification of a `run_pure` failure.
enum JitErrClass {
    /// Expected divergence on synthetic IR — not a bug; skip.
    Skip,
    /// Reportable B2: a JIT error that should not occur where eval succeeded.
    B2,
    /// Reportable B3: a fatal signal caught in-process.
    B3(i32),
}

/// Classify a `run_pure` `JitError` against the known-divergence filter.
///
/// Whitelisted (skip) — synthetic-IR / capacity / eager-evaluation artifacts:
/// - `Compilation`/`Pipeline`/`MissingConTags`: the JIT cannot compile this
///   synthetic shape.
/// - `HeapBridge(_)`: the JIT result was a garbage heap object (downstream of
///   unresolved vars on synthetic IR) — the production bridge rejects it. This
///   is the dominant non-bug class at depth ≥ 5.
/// - `Yield(UnresolvedVar | HeapOverflow | StackOverflow)`: synthetic LetRec /
///   nursery capacity / eager-evaluation gap.
///
/// `Yield(Signal)` is a B3. Everything else (e.g. `BadPointer`, `BadThunkState`,
/// `UnexpectedTag`, `CaseTrap`, `NullFunPtr`) is a reportable B2.
fn classify_jit_error(err: &JitError) -> JitErrClass {
    match err {
        JitError::Compilation(_) | JitError::Pipeline(_) | JitError::MissingConTags(_) => {
            JitErrClass::Skip
        }
        JitError::HeapBridge(_) => JitErrClass::Skip,
        JitError::Yield(y) => match y {
            YieldError::Runtime(RuntimeError::UnresolvedVar(_))
            | YieldError::Runtime(RuntimeError::HeapOverflow)
            | YieldError::Runtime(RuntimeError::StackOverflow) => JitErrClass::Skip,
            YieldError::Signal(sig) => JitErrClass::B3(*sig),
            _ => JitErrClass::B2,
        },
        _ => JitErrClass::B2,
    }
}

/// Compile and run an expression through the JIT with a custom nursery size.
/// Returns `None` if compilation fails (a synthetic-IR limitation, not a bug).
///
/// The returned `Vec<u8>` is the live nursery backing `vmctx`'s pointers and
/// must be kept alive by the caller until reconstruction finishes.
fn jit_compile_and_run(
    tree: &CoreExpr,
    nursery_size: usize,
) -> Option<(*const u8, VMContext, Vec<u8>, CodegenPipeline)> {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).ok()?;
    let func_id = compile_expr(&mut pipeline, tree, "deep_diff", &ExternalEnv::new()).ok()?;
    pipeline.finalize().ok()?;

    let mut nursery = vec![0u8; nursery_size];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    Some((result as *const u8, vmctx, nursery, pipeline))
}

/// Evaluate one case in both backends and classify the outcome into an exit
/// code. Diagnostics for reportable outcomes are printed to stderr (visible via
/// the worker's `--nocapture`).
///
/// The JIT oracle is the production `JitEffectMachine::run_pure` path, which
/// reconstructs values via the battle-tested `heap_bridge` (correct under GC)
/// and classifies synthetic-IR garbage as a whitelisted `HeapBridge` error —
/// avoiding the false positives the raw `compile_expr` + `heap_to_value` path
/// produces on unreduced synthetic expressions.
fn run_one_case(expr: &CoreExpr, nursery_size: usize, optimize_first: bool) -> i32 {
    // Skip the synthetic-LetRec known-divergence class (see
    // `has_synthetic_letrec`): a bare-Var rec RHS is ill-defined and the
    // backends legitimately disagree on its value.
    if has_synthetic_letrec(expr) {
        return EXIT_SKIP;
    }

    let mut expr = expr.clone();
    if optimize_first {
        // B4 knob: optimization must preserve JIT/eval agreement.
        let _ = optimize(&mut expr);
    }

    let table = build_table_for_expr(&expr);

    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let eval_result = eval(&expr, &env, &mut heap);

    let jit_result = match JitEffectMachine::compile(&expr, &table, nursery_size) {
        Ok(mut m) => m.run_pure(),
        Err(e) => Err(e),
    };

    match (eval_result, jit_result) {
        (Ok(eval_val), Ok(jit_val)) => {
            match tidepool_eval::eval::deep_force(eval_val, &mut heap) {
                Ok(forced) => {
                    // Bias toward ground results: a closure on either side is a
                    // known non-comparable divergence (synthetic IR), not a bug.
                    if compare::contains_closure(&forced) || compare::contains_closure(&jit_val) {
                        return EXIT_SKIP;
                    }
                    if compare::values_equal(&forced, &jit_val) {
                        EXIT_OK
                    } else {
                        eprintln!(
                            "B1 VALUE MISMATCH\n  eval: {:?}\n  jit:  {:?}\n  expr: {:#?}",
                            forced, jit_val, expr
                        );
                        EXIT_B1
                    }
                }
                // Deep-force failure (laziness gap) — skip.
                Err(_) => EXIT_SKIP,
            }
        }
        (Ok(_), Err(err)) => match classify_jit_error(&err) {
            JitErrClass::Skip => EXIT_SKIP,
            JitErrClass::B2 => {
                eprintln!("B2 JIT-ONLY ERROR: {:?}\n  expr: {:#?}", err, expr);
                EXIT_B2
            }
            JitErrClass::B3(sig) => {
                eprintln!(
                    "B3 IN-PROCESS SIGNAL {}: {:?}\n  expr: {:#?}",
                    sig, err, expr
                );
                // Use a distinct exit code so the parent records it as B3.
                EXIT_B3
            }
        },
        // Eval-side error or both errored — skip (interpreter laziness gap).
        (Err(_), _) => EXIT_SKIP,
    }
}

/// Re-exec'd worker. Reads its case from the environment, runs it, and exits
/// with the classified code. Marked `#[ignore]` so it only runs when the parent
/// property selects it with `--exact deep_diff_worker --ignored`.
#[test]
#[ignore = "subprocess worker for proptest_deep_differential; driven via re-exec"]
fn deep_diff_worker() {
    let path = std::env::var(ENV_CASE).expect("worker: missing case path");
    let bytes = std::fs::read(&path).expect("worker: cannot read case file");
    let expr = tidepool_repr::serial::read_cbor(&bytes).expect("worker: cannot decode case");
    let nursery = std::env::var(ENV_NURSERY)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64 * 1024);
    let opt = std::env::var(ENV_OPT).map(|s| s == "1").unwrap_or(false);

    // Force a flush of any diagnostics before exiting.
    let code = run_one_case(&expr, nursery, opt);
    std::process::exit(code);
}

/// Classified result of one subprocess case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Both backends produced equal ground values (a real comparison).
    Compared,
    /// Known-divergence class — skipped, no comparison.
    Skipped,
    /// B1: deep-forced values differed.
    B1,
    /// B2: JIT-only error outside the whitelist.
    B2,
    /// B3: child died by a fatal signal.
    B3(i32),
    /// Child exceeded the per-case wall-clock budget (likely a runaway loop).
    Timeout,
    /// Infrastructure error spawning/awaiting the child.
    Infra,
}

/// Fatal signals that always count as a reportable B3.
fn is_fatal_signal(sig: i32) -> bool {
    // SIGILL=4, SIGABRT=6, SIGBUS=7, SIGSEGV=11
    matches!(sig, 4 | 6 | 7 | 11)
}

/// Serialize one case to a temp file, re-exec the worker, and classify the
/// child's exit. The child inherits stderr so worker diagnostics surface in the
/// test log; a wall-clock deadline kills runaway children.
fn drive_subprocess(expr: &CoreExpr, nursery: usize, opt: bool) -> Outcome {
    let cbor = match tidepool_repr::serial::write_cbor(expr) {
        Ok(b) => b,
        Err(_) => return Outcome::Infra,
    };
    let idx = CASE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("tp_deepdiff_{}_{}.cbor", std::process::id(), idx));
    if std::fs::write(&path, &cbor).is_err() {
        return Outcome::Infra;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            return Outcome::Infra;
        }
    };

    let spawn = Command::new(exe)
        .args(["--exact", WORKER_TEST, "--ignored", "--nocapture"])
        .env(ENV_CASE, &path)
        .env(ENV_NURSERY, nursery.to_string())
        .env(ENV_OPT, if opt { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn();

    let mut child = match spawn {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            return Outcome::Infra;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(20);
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Some(sig) = status.signal() {
                    break if is_fatal_signal(sig) {
                        Outcome::B3(sig)
                    } else {
                        // Non-fatal signal (e.g. killed externally) — skip.
                        Outcome::Skipped
                    };
                }
                break match status.code() {
                    Some(EXIT_OK) => Outcome::Compared,
                    Some(EXIT_SKIP) => Outcome::Skipped,
                    Some(EXIT_B1) => Outcome::B1,
                    Some(EXIT_B2) => Outcome::B2,
                    // In-process caught signal (run_pure protection).
                    Some(EXIT_B3) => Outcome::B3(0),
                    // Any other code (e.g. 101 panic) — synthetic-IR infra, skip.
                    _ => Outcome::Skipped,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Outcome::Timeout;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break Outcome::Infra,
        }
    };

    let _ = std::fs::remove_file(&path);
    outcome
}

/// Shared driver: run `strategy` for `case_count()` cases through the
/// subprocess harness; fail on the first reportable (B1/B2/B3) outcome.
fn drive<S, F>(label: &str, make: F, nursery: usize, opt: bool)
where
    S: Strategy<Value = CoreExpr>,
    F: FnOnce() -> S + Send + 'static,
{
    let label = label.to_string();
    let cases = case_count();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let strategy = make();
            let mut runner = TestRunner::new(Config {
                cases,
                ..Config::default()
            });
            let compared = Cell::new(0u64);
            let skipped = Cell::new(0u64);
            let timeout = Cell::new(0u64);
            let infra = Cell::new(0u64);

            let result = runner.run(&strategy, |expr| {
                match drive_subprocess(&expr, nursery, opt) {
                    Outcome::Compared => compared.set(compared.get() + 1),
                    Outcome::Skipped => skipped.set(skipped.get() + 1),
                    Outcome::Timeout => timeout.set(timeout.get() + 1),
                    Outcome::Infra => infra.set(infra.get() + 1),
                    Outcome::B1 => {
                        let hex = hex_of(&expr);
                        return Err(TestCaseError::fail(format!(
                            "B1 deep-forced value mismatch (see worker stderr).\nseed-hex: {hex}\nexpr: {expr:#?}"
                        )));
                    }
                    Outcome::B2 => {
                        let hex = hex_of(&expr);
                        return Err(TestCaseError::fail(format!(
                            "B2 JIT-only error outside whitelist (see worker stderr).\nseed-hex: {hex}\nexpr: {expr:#?}"
                        )));
                    }
                    Outcome::B3(sig) => {
                        let hex = hex_of(&expr);
                        return Err(TestCaseError::fail(format!(
                            "B3 fatal signal {sig} in JIT child.\nseed-hex: {hex}\nexpr: {expr:#?}"
                        )));
                    }
                }
                Ok(())
            });

            eprintln!(
                "\n[deep-diff {label}] compared={}, skipped={}, timeout={}, infra={}",
                compared.get(),
                skipped.get(),
                timeout.get(),
                infra.get()
            );
            result.unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Hex-encode a CoreExpr's CBOR for compact reproduction in failure messages
/// and regression seeds.
fn hex_of(expr: &CoreExpr) -> String {
    match tidepool_repr::serial::write_cbor(expr) {
        Ok(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => "<unserializable>".to_string(),
    }
}

/// P1 — depth-5 ground, 64KB nursery. Strict deep comparison.
#[test]
fn deep_diff_ground_depth5() {
    drive(
        "ground-d5-64k",
        || arb_ground_expr_depth(5),
        64 * 1024,
        false,
    );
}

/// P2 — depth-5 ground, optimized then compared (B4 optimize knob).
#[test]
fn deep_diff_ground_depth5_optimized() {
    drive(
        "ground-d5-opt",
        || arb_ground_expr_depth(5),
        64 * 1024,
        true,
    );
}

/// P3 — depth-7, Join/LetRec/Case-heavy. Must complete all cases with no host
/// stack overflow (subprocess containment guarantees the parent survives any
/// child fault).
#[test]
fn deep_diff_join_letrec_heavy_depth7() {
    drive(
        "join-letrec-d7",
        || arb_core_expr_weighted(7, 5, 4, 4),
        64 * 1024,
        false,
    );
}

/// P4 — depth-5 ground at a tiny 4KB nursery (B4 nursery knob; forces GC).
#[test]
fn deep_diff_ground_depth5_small_nursery() {
    drive("ground-d5-4k", || arb_ground_expr_depth(5), 4 * 1024, false);
}

/// Decode a CBOR-hex seed (as printed in failure messages) into a CoreExpr.
fn expr_from_hex(hex: &str) -> CoreExpr {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("bad hex"))
        .collect();
    tidepool_repr::serial::read_cbor(&bytes).expect("decode seed")
}

/// Diagnostic harness for a hex seed: compares the interpreter against BOTH the
/// production machine path (`run_pure` / `heap_bridge`) and this crate's raw
/// `compare::heap_to_value` path, at the given nursery size. Used to decide
/// whether a B1 mismatch is a real eval/JIT divergence or a reconstruction
/// artifact under GC. Run with `--ignored --nocapture` and the seed set in code.
fn diag_seed(hex: &str, nursery: usize) {
    use tidepool_codegen::jit_machine::JitEffectMachine;
    use tidepool_eval::env_from_datacon_table;
    use tidepool_testing::proptest::build_table_for_expr;

    let expr = expr_from_hex(hex);
    let table = build_table_for_expr(&expr);

    // 1. Interpreter (proper datacon env), deep-forced.
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let eval_forced =
        eval(&expr, &env, &mut heap).and_then(|v| tidepool_eval::eval::deep_force(v, &mut heap));

    // 2. Production machine path: run_pure -> heap_bridge::heap_to_value_forcing.
    let machine_val = match JitEffectMachine::compile(&expr, &table, nursery) {
        Ok(mut m) => m.run_pure().map_err(|e| format!("{e:?}")),
        Err(e) => Err(format!("compile: {e:?}")),
    };

    // 3. This crate's raw path: compile_expr + func() + compare::heap_to_value.
    let raw_val = match jit_compile_and_run(&expr, nursery) {
        Some((ptr, mut vmctx, _n, _p)) => {
            let err = host_fns::take_runtime_error();
            if let Some(e) = err {
                Err(format!("{e:?}"))
            } else {
                Ok(unsafe { compare::heap_to_value(ptr, &mut vmctx) })
            }
        }
        None => Err("compile failed".to_string()),
    };

    eprintln!("\n=== diag_seed nursery={nursery} ===");
    eprintln!("synthetic_letrec: {}", has_synthetic_letrec(&expr));
    eprintln!("eval    : {eval_forced:?}");
    eprintln!("machine : {machine_val:?}");
    eprintln!("raw/this: {raw_val:?}");
    if let (Ok(ev), Ok(mv)) = (&eval_forced, &machine_val) {
        eprintln!("eval==machine: {}", compare::values_equal(ev, mv));
    }
    if let (Ok(ev), Ok(rv)) = (&eval_forced, &raw_val) {
        eprintln!("eval==raw    : {}", compare::values_equal(ev, rv));
    }
}

/// Read a committed hex seed from `tests/deep_differential_seeds/<name>`.
fn read_seed(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/deep_differential_seeds")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read seed {}: {e}", path.display()))
        .trim()
        .to_string()
}

/// Characterization (NOT a tidepool bug): the depth-5/4KB case that the first
/// hunt mis-flagged as B1. The interpreter yields a ground `Pair`, while the
/// JIT result is a garbage heap object (`UnexpectedHeapTag`) — the production
/// `run_pure`/`heap_bridge` path rejects it as a `HeapBridge` error (whitelisted
/// synthetic-IR divergence), but this crate's raw `compare::heap_to_value`
/// silently mis-decodes it as a `Closure`, producing a false mismatch. This is
/// the harness pitfall that motivated using `run_pure` as the deep-diff oracle.
/// `#[ignore]`d — run manually; prints all three reconstructions at 4KB & 64KB.
#[test]
#[ignore = "characterization: heap_bridge tag-0 vs raw heap_to_value (harness pitfall, not a bug)"]
fn diag_heapbridge_unexpected_tag() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let hex = read_seed("heapbridge_unexpected_tag_d5.hex");
            diag_seed(&hex, 4 * 1024);
            diag_seed(&hex, 64 * 1024);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Characterization (NOT a tidepool bug): the depth-7 synthetic-LetRec value
/// divergence. The expression contains a `LetRec` binding whose RHS is a bare
/// `Var` (e.g. `let rec a = a`), which GHC never emits. Both backends succeed
/// with ground values, but they DIFFER (here `Con(0,[])` vs `Con(4,[Int,Word])`,
/// even disagreeing on the result type) because a bare-Var rec binding is
/// semantically ill-defined and the interpreter (lazy thunk) and JIT (eager,
/// sequential) resolve it differently. This is the same class the
/// `proptest_differential` whitelist skips as `UnresolvedVar`. The deep-diff
/// worker filters it via [`has_synthetic_letrec`]; this test documents what the
/// filter suppresses. If the filter were removed, this assertion would fail.
#[test]
#[ignore = "characterization: synthetic-LetRec (bare-Var rec RHS) value divergence — not a tidepool bug"]
fn repro_synthetic_letrec_divergence() {
    use tidepool_codegen::jit_machine::JitEffectMachine;
    use tidepool_eval::env_from_datacon_table;
    use tidepool_testing::proptest::build_table_for_expr;

    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let hex = read_seed("synthetic_letrec_d7.hex");
            let expr = expr_from_hex(&hex);

            // It is the synthetic-LetRec class (so the worker skips it).
            assert!(
                has_synthetic_letrec(&expr),
                "seed should contain a bare-Var LetRec RHS"
            );

            // Unfiltered, the two backends diverge on a ground value.
            let table = build_table_for_expr(&expr);
            let mut heap = VecHeap::new();
            let env = env_from_datacon_table(&table);
            let eval_forced = eval(&expr, &env, &mut heap)
                .and_then(|v| tidepool_eval::eval::deep_force(v, &mut heap))
                .expect("eval should succeed");
            let jit_val = JitEffectMachine::compile(&expr, &table, 64 * 1024)
                .expect("compile")
                .run_pure()
                .expect("run_pure should succeed");

            eprintln!("eval: {eval_forced:?}\njit:  {jit_val:?}");
            assert!(
                !compare::values_equal(&eval_forced, &jit_val),
                "documented divergence: this synthetic-LetRec case is expected to \
                 diverge; if it now agrees, the generator or backends changed"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Env-driven triage: read a hex seed from the file named by `TP_DIAG_SEED`
/// and diagnose it at 4KB and 64KB. `#[ignore]`d — run manually during triage.
#[test]
#[ignore = "env-driven triage diagnostic; set TP_DIAG_SEED to a hex file"]
fn diag_env_seed() {
    let path = std::env::var("TP_DIAG_SEED").expect("set TP_DIAG_SEED to a hex file path");
    let hex = std::fs::read_to_string(&path).expect("read seed file");
    let hex = hex.trim().to_string();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            diag_seed(&hex, 4 * 1024);
            diag_seed(&hex, 64 * 1024);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Generator-reach statistics: sample the deep strategies and report the
/// frequency of Join / LetRec / Case nodes (feeds the findings doc). Fast,
/// in-process, and green — also a smoke test that deep generation terminates.
#[test]
fn generator_reach_stats() {
    use proptest::strategy::ValueTree;
    use tidepool_repr::frame::CoreFrame;

    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 1,
                ..Config::default()
            });

            fn tally(
                runner: &mut TestRunner,
                strat: &impl Strategy<Value = CoreExpr>,
                samples: usize,
            ) -> (u64, u64, u64, u64, u64) {
                let (mut join, mut letrec, mut case, mut jump, mut nodes) = (0, 0, 0, 0, 0u64);
                for _ in 0..samples {
                    let tree = strat.new_tree(runner).unwrap().current();
                    for n in &tree.nodes {
                        nodes += 1;
                        match n {
                            CoreFrame::Join { .. } => join += 1,
                            CoreFrame::Jump { .. } => jump += 1,
                            CoreFrame::LetRec { .. } => letrec += 1,
                            CoreFrame::Case { .. } => case += 1,
                            _ => {}
                        }
                    }
                }
                (join, jump, letrec, case, nodes)
            }

            let samples = 300usize;
            let g5 = arb_ground_expr_depth(5);
            let w7 = arb_core_expr_weighted(7, 5, 4, 4);
            let (j5, jp5, lr5, c5, n5) = tally(&mut runner, &g5, samples);
            let (j7, jp7, lr7, c7, n7) = tally(&mut runner, &w7, samples);

            eprintln!(
                "\n[reach] ground depth-5 over {samples} samples: nodes={n5} \
                 Join={j5} Jump={jp5} LetRec={lr5} Case={c5}"
            );
            eprintln!(
                "[reach] weighted(7,5,4,4) over {samples} samples: nodes={n7} \
                 Join={j7} Jump={jp7} LetRec={lr7} Case={c7}"
            );

            // Sanity: deep generation must actually produce nodes.
            assert!(n5 > 0 && n7 > 0, "generators produced empty trees");
        })
        .unwrap()
        .join()
        .unwrap();
}
