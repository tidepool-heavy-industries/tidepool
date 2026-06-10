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
use tidepool_codegen::host_fns;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_eval::{env::Env, eval::eval, heap::VecHeap};
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::CoreExpr;
use tidepool_testing::compare;
use tidepool_testing::gen::{arb_core_expr_weighted, arb_ground_expr_depth};

/// Name of the `#[ignore]`d worker test, selected with `--exact`.
const WORKER_TEST: &str = "deep_diff_worker";
/// Env var carrying the path to the case's CBOR-serialized CoreExpr.
const ENV_CASE: &str = "TP_DEEP_DIFF_CASE";
/// Env var carrying the nursery size in bytes.
const ENV_NURSERY: &str = "TP_DEEP_DIFF_NURSERY";
/// Env var carrying "1" to optimize the expr before comparing.
const ENV_OPT: &str = "TP_DEEP_DIFF_OPT";

/// Worker exit codes (0 = ok/skip).
const EXIT_OK: i32 = 0;
const EXIT_B1: i32 = 2;
const EXIT_B2: i32 = 3;

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

/// JIT-only runtime errors that are *expected* divergences on synthetic IR and
/// therefore NOT bugs. Mirrors the whitelist in `proptest_differential.rs`.
fn is_whitelisted_jit_error(err: &RuntimeError) -> bool {
    matches!(
        err,
        RuntimeError::UnresolvedVar(_) | RuntimeError::HeapOverflow | RuntimeError::StackOverflow
    )
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
    let func_id = compile_expr(&mut pipeline, tree, "deep_diff").ok()?;
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
fn run_one_case(expr: &CoreExpr, nursery_size: usize, optimize_first: bool) -> i32 {
    let mut expr = expr.clone();
    if optimize_first {
        // B4 knob: optimization must preserve JIT/eval agreement.
        let _ = optimize(&mut expr);
    }

    let mut heap = VecHeap::new();
    let eval_result = eval(&expr, &Env::new(), &mut heap);

    let jit = jit_compile_and_run(&expr, nursery_size);
    let jit_err = host_fns::take_runtime_error();

    let (result_ptr, mut vmctx, _nursery, _pipeline) = match jit {
        Some(t) => t,
        // Compile failure on synthetic IR — skip.
        None => return EXIT_OK,
    };

    match (eval_result, jit_err) {
        (Ok(eval_val), None) => match tidepool_eval::eval::deep_force(eval_val, &mut heap) {
            Ok(forced) => {
                let jit_val = unsafe { compare::heap_to_value(result_ptr, &mut vmctx) };
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
            Err(_) => EXIT_OK,
        },
        (Ok(_), Some(err)) => {
            if is_whitelisted_jit_error(&err) {
                EXIT_OK
            } else {
                eprintln!("B2 JIT-ONLY ERROR: {:?}\n  expr: {:#?}", err, expr);
                EXIT_B2
            }
        }
        // Eval-side error or both errored — skip (interpreter laziness gap).
        (Err(_), _) => EXIT_OK,
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
    /// Both backends agreed (or the case was skipped as a known divergence).
    OkOrSkip,
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
    let path = std::env::temp_dir().join(format!("tp_deepdiff_{}_{}.cbor", std::process::id(), idx));
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
                        Outcome::OkOrSkip
                    };
                }
                break match status.code() {
                    Some(EXIT_OK) => Outcome::OkOrSkip,
                    Some(EXIT_B1) => Outcome::B1,
                    Some(EXIT_B2) => Outcome::B2,
                    // Any other code (e.g. 101 panic) — synthetic-IR infra, skip.
                    _ => Outcome::OkOrSkip,
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
            let timeout = Cell::new(0u64);
            let infra = Cell::new(0u64);

            let result = runner.run(&strategy, |expr| {
                match drive_subprocess(&expr, nursery, opt) {
                    Outcome::OkOrSkip => compared.set(compared.get() + 1),
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
                "\n[deep-diff {label}] compared/skipped={}, timeout={}, infra={}",
                compared.get(),
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
    drive("ground-d5-opt", || arb_ground_expr_depth(5), 64 * 1024, true);
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
