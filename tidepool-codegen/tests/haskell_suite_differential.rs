//! Haskell fixture differential testing (Interpreter vs JIT).
//!
//! For each CBOR fixture in haskell/test/suite_cbor/, evaluate with both the
//! interpreter and JIT, and verify they produce the same result.
//! This tests the full pipeline on real GHC-compiled code.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::machine_state::MachineState;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::serial::read::read_cbor;
use tidepool_testing::compare;
use tidepool_testing::haskell_suite::suite_table as table;

/// Fixtures to skip — known to use features the JIT doesn't support for
/// standalone execution (e.g., unresolved external bindings, string ops).
fn should_skip(name: &str) -> bool {
    // Skip $-prefixed GHC internal bindings
    if name.starts_with('$') {
        return true;
    }
    // Skip meta.cbor (metadata, not an expression)
    if name == "meta" {
        return true;
    }
    // Lifted locals (`_u<digits>`) are NOT skipped here, unlike the real-Core
    // corpus harness. This suite's fixtures are the test suite's own bindings,
    // and 75 of its lifted locals compare cleanly — excluding the class to
    // silence two pathological ones costs an order of magnitude more coverage
    // than it buys. The two are named in EXPECTED_EVAL_JIT_DIVERGE instead.
    false
}

/// Fixtures where BOTH engines legitimately error, with the one-line reason.
/// Populated from a real run's `both_error_names` — never guessed. A
/// both-error fixture not on this list fails the gate; a listed fixture that
/// now compares cleanly is reported (not failed) so the stale entry can be
/// pruned.
const EXPECTED_BOTH_ERROR: &[(&str, &str)] = &[];

/// Fixtures where eval and JIT legitimately land on different outcomes (eval
/// errors, JIT produces a value), with the one-line reason. Same
/// allow-or-fail contract as `EXPECTED_BOTH_ERROR`.
const EXPECTED_EVAL_JIT_DIVERGE: &[(&str, &str)] = &[
    (
        "thunk_blackhole",
        "GHC Core `thunk_blackhole = let x = x in x` (the real-GHC shape #336's \
         blackhole_differential.rs cites). eval correctly rejects the \
         self-reference as a BlackHole (InfiniteLoop). This suite drives \
         CodegenPipeline::compile_expr directly, not JitEffectMachine::run_pure \
         (the path blackhole_differential.rs pins to the same contract for a \
         synthetic LetRec{x=Var(x)} shape) — on the real top-level-lifted Core \
         form the direct-compile path does not raise runtime_blackhole_trap and \
         returns a value instead. A real JIT gap specific to this execution \
         path, left unfixed here (production code is out of scope for this \
         gate-hardening change).",
    ),
    (
        "xs_u8286623314361975231",
        "GHC-lifted local helper bound to a self-referential, effectively \
         unbounded list; standalone execution forces it outside the call site \
         that would bound it. eval's deep_force walks it fully and hits its own \
         recursion-depth guard (DepthLimit); the JIT side goes through \
         compare::heap_to_value, which silently truncates past MAX_HEAP_DEPTH \
         (1000) instead of erroring — an asymmetry between the two forcing \
         strategies on an out-of-context fixture, not a real engine divergence. \
         Named individually rather than excluded as a class: this suite's other \
         lifted locals compare cleanly and are real coverage. (Name carries a \
         GHC-minted content hash suffix that shifts on every corpus \
         regeneration — the shape and cause are what's pinned, not the exact \
         name.)",
    ),
    (
        "xs'_u8286623314361975295",
        "Same shape and cause as xs_u8286623314361975231 — lifted-local \
         unbounded list, deep_force DepthLimit vs heap_to_value's silent \
         MAX_HEAP_DEPTH truncation.",
    ),
];

/// A nontrivial floor on how many fixtures must reach a clean comparison.
/// Observed 312 on a baseline run (tested=349, closure_skip=34, mismatch=0,
/// both_error=0, jit_only_error=0, eval_jit_diverge=3, skipped=1) — set a
/// little below that so ordinary fixture churn doesn't flap the gate, while a
/// real collapse in comparison reach still fails it.
const COMPARED_FLOOR: usize = 300;

#[test]
#[ignore = "expensive: forks a full suite_cbor differential pass (eval + JIT \
            per fixture); run with TIDEPOOL_EXPENSIVE_TESTS=1 \
            cargo nextest run -p tidepool-codegen --run-ignored all \
            -E 'test(haskell_suite_differential)'"]
fn haskell_suite_differential() {
    if std::env::var("TIDEPOOL_EXPENSIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("SKIPPED (expensive): set TIDEPOOL_EXPENSIVE_TESTS=1 to run");
        return;
    }
    tidepool_testing::watchdog::arm();
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let cbor_dir =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/suite_cbor");

            let table = table();
            let env = env_from_datacon_table(&table);

            let mut tested = 0;
            let mut skipped = 0;
            let mut compared = 0;
            let mut closure_skip = 0;
            let mut mismatch = 0;
            let mut both_error = 0;
            let mut jit_only_error = 0;
            let mut eval_jit_diverge = 0;
            let mut mismatch_names: Vec<String> = Vec::new();
            let mut both_error_names: Vec<String> = Vec::new();
            let mut jit_only_error_names: Vec<String> = Vec::new();
            let mut eval_jit_diverge_names: Vec<String> = Vec::new();

            for entry in std::fs::read_dir(&cbor_dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("cbor") {
                    continue;
                }

                let name = path.file_stem().unwrap().to_str().unwrap().to_string();

                if should_skip(&name) {
                    skipped += 1;
                    continue;
                }

                let _guard = tidepool_testing::watchdog::begin(&name);

                let bytes = std::fs::read(&path).unwrap();
                // One current format: an unreadable fixture is corpus rot,
                // never silently reduced coverage.
                let expr =
                    read_cbor(&bytes).unwrap_or_else(|e| panic!("{name}: fixture unreadable: {e}"));

                // Interpreter
                let mut heap = VecHeap::new();
                let eval_result = eval(&expr, &env, &mut heap);
                let eval_forced = eval_result.and_then(|v| deep_force(v, &mut heap));

                // JIT (catch panics)
                let jit_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).ok()?;
                    let func_id =
                        compile_expr(&mut pipeline, &expr, "suite_test", &ExternalEnv::new())
                            .ok()?;
                    pipeline.finalize().ok()?;

                    let mut nursery = vec![0u8; 1 << 20]; // 1MB nursery for real programs
                    let start = nursery.as_mut_ptr();
                    let end = unsafe { start.add(nursery.len()) };
                    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);
                    let machine_state = MachineState::new();
                    vmctx.machine_state =
                        &machine_state as *const MachineState as *mut MachineState;

                    machine_state.set_gc_state(start, nursery.len());
                    machine_state.set_stack_map_registry(&pipeline.stack_maps);

                    let ptr = pipeline.get_function_ptr(func_id);
                    let func: unsafe extern "C" fn(*mut VMContext) -> i64 =
                        unsafe { std::mem::transmute(ptr) };
                    let result_ptr = unsafe { func(&mut vmctx as *mut VMContext) } as *const u8;

                    let val = unsafe { compare::heap_to_value(result_ptr, &mut vmctx) };
                    Some((val, nursery, pipeline))
                }));

                tested += 1;

                match (&eval_forced, &jit_result) {
                    (Ok(eval_val), Ok(Some((jit_val, _, _)))) => {
                        // Haskell fixtures can evaluate to closures — keep closure checks here
                        if !compare::contains_closure(eval_val)
                            && !compare::contains_closure(jit_val)
                        {
                            if compare::values_equal(eval_val, jit_val) {
                                compared += 1;
                            } else {
                                mismatch += 1;
                                mismatch_names.push(name.clone());
                                eprintln!("MISMATCH {}: eval={} jit={}", name, eval_val, jit_val);
                            }
                        } else {
                            closure_skip += 1;
                        }
                    }
                    (Err(eval_err), Err(_)) | (Err(eval_err), Ok(None)) => {
                        both_error += 1;
                        both_error_names.push(name.clone());
                        eprintln!("BOTH_ERROR {}: eval={:?}", name, eval_err);
                    }
                    (Ok(_), Err(_)) | (Ok(_), Ok(None)) => {
                        jit_only_error += 1;
                        jit_only_error_names.push(name.clone());
                        eprintln!("JIT_ONLY_ERROR {}", name);
                    }
                    (Err(eval_err), Ok(Some(_))) => {
                        eval_jit_diverge += 1;
                        eval_jit_diverge_names.push(name.clone());
                        eprintln!("EVAL_JIT_DIVERGE {}: eval={:?}", name, eval_err);
                    }
                }
            }

            eprintln!(
                "\nHaskell suite differential: tested={tested}, compared={compared}, \
                 closure_skip={closure_skip}, mismatch={mismatch}, \
                 both_error={both_error}, jit_only_error={jit_only_error}, \
                 eval_jit_diverge={eval_jit_diverge}, skipped={skipped}"
            );
            eprintln!("mismatch_names: {mismatch_names:?}");
            eprintln!("both_error_names: {both_error_names:?}");
            eprintln!("jit_only_error_names: {jit_only_error_names:?}");
            eprintln!("eval_jit_diverge_names: {eval_jit_diverge_names:?}");

            let mut violations: Vec<String> = Vec::new();

            for n in &mismatch_names {
                violations.push(format!("MISMATCH {n}: engines produced different values"));
            }
            for n in &jit_only_error_names {
                violations.push(format!(
                    "JIT_ONLY_ERROR {n}: eval succeeded, JIT errored — unexpected"
                ));
            }
            for n in &both_error_names {
                if !EXPECTED_BOTH_ERROR.iter().any(|(fx, _)| fx == n) {
                    violations.push(format!(
                        "BOTH_ERROR {n}: both engines errored, and {n} is not on \
                         EXPECTED_BOTH_ERROR"
                    ));
                }
            }
            for n in &eval_jit_diverge_names {
                if !EXPECTED_EVAL_JIT_DIVERGE.iter().any(|(fx, _)| fx == n) {
                    violations.push(format!(
                        "EVAL_JIT_DIVERGE {n}: eval errored but JIT produced a value, and \
                         {n} is not on EXPECTED_EVAL_JIT_DIVERGE"
                    ));
                }
            }
            if compared < COMPARED_FLOOR {
                violations.push(format!(
                    "compared={compared} fell below COMPARED_FLOOR={COMPARED_FLOOR}"
                ));
            }

            // A listed fixture that no longer reproduces is fine — report it so
            // the stale entry can be pruned, never fail the gate over it.
            for (fx, _) in EXPECTED_BOTH_ERROR {
                if !both_error_names.iter().any(|n| n == fx) {
                    eprintln!(
                        "NOTE: EXPECTED_BOTH_ERROR entry {fx} did not both-error this run \
                         — stale, consider pruning"
                    );
                }
            }
            for (fx, _) in EXPECTED_EVAL_JIT_DIVERGE {
                if !eval_jit_diverge_names.iter().any(|n| n == fx) {
                    eprintln!(
                        "NOTE: EXPECTED_EVAL_JIT_DIVERGE entry {fx} did not diverge this run \
                         — stale, consider pruning"
                    );
                }
            }

            assert!(
                violations.is_empty(),
                "{} unexpected outcome(s):\n{}",
                violations.len(),
                violations.join("\n")
            );
        })
        .unwrap();
    handle.join().unwrap();
}
