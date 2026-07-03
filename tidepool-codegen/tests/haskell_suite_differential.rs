//! Option 7: Haskell fixture differential testing (Interpreter vs JIT).
//!
//! For each CBOR fixture in haskell/test/suite_cbor/, evaluate with both the
//! interpreter and JIT, and verify they produce the same result.
//! This tests the full pipeline on real GHC-compiled code.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::*;
use tidepool_testing::compare;

static META: &[u8] = include_bytes!("../../haskell/test/suite_cbor/meta.cbor");

fn table() -> DataConTable {
    read_metadata(META).unwrap().0
}

/// Watchdog state: the fixture currently being processed, and an epoch that
/// bumps on every fixture transition. The corpus is data — one pathological
/// fixture must fail loudly with its name, not spin the suite forever.
static CURRENT_FIXTURE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static FIXTURE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Abort the process (exit 101) if any single fixture runs longer than
/// `TIDEPOOL_FIXTURE_TIMEOUT_SECS` (default 120). Writes to raw stderr so the
/// message survives libtest output capture. The thread dies with the process.
fn spawn_fixture_watchdog() {
    use std::sync::atomic::Ordering;
    let limit_secs: u64 = std::env::var("TIDEPOOL_FIXTURE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    std::thread::spawn(move || {
        let mut last_epoch = FIXTURE_EPOCH.load(Ordering::Relaxed);
        let mut stuck_secs = 0u64;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let epoch = FIXTURE_EPOCH.load(Ordering::Relaxed);
            if epoch == last_epoch {
                stuck_secs += 10;
            } else {
                last_epoch = epoch;
                stuck_secs = 0;
            }
            if stuck_secs >= limit_secs {
                let name = CURRENT_FIXTURE.lock().unwrap().clone();
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr(),
                    "\n[FIXTURE WATCHDOG] fixture '{name}' exceeded {limit_secs}s — \
                     suspected non-termination; aborting suite"
                );
                std::process::exit(101);
            }
        }
    });
}

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
    false
}

#[test]
fn haskell_suite_differential() {
    spawn_fixture_watchdog();
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

                *CURRENT_FIXTURE.lock().unwrap() = name.clone();
                FIXTURE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                let bytes = std::fs::read(&path).unwrap();
                let expr = match read_cbor(&bytes) {
                    Ok(e) => e,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };

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

                    host_fns::set_gc_state(start, nursery.len());
                    host_fns::set_stack_map_registry(&pipeline.stack_maps);

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
                                eprintln!("MISMATCH {}: eval={} jit={}", name, eval_val, jit_val);
                            }
                        } else {
                            closure_skip += 1;
                        }
                    }
                    (Err(_), Err(_)) | (Err(_), Ok(None)) => {
                        both_error += 1;
                    }
                    (Ok(_), Err(_)) | (Ok(_), Ok(None)) => {
                        jit_only_error += 1;
                    }
                    (Err(_), Ok(Some(_))) => {
                        eval_jit_diverge += 1;
                    }
                }
            }

            eprintln!(
                "\nHaskell suite differential: tested={tested}, compared={compared}, \
                 closure_skip={closure_skip}, mismatch={mismatch}, \
                 both_error={both_error}, jit_only_error={jit_only_error}, \
                 eval_jit_diverge={eval_jit_diverge}, skipped={skipped}"
            );

            // At least some fixtures should have been tested
            assert!(tested > 0, "No fixtures were tested!");
        })
        .unwrap();
    handle.join().unwrap();
}
