//! Ledger #39(a) regression guard: sibling-alt refined-dictionary methods.
//!
//! A GADT whose alternatives refine the type index (`KInt :: K Int`,
//! `KPrec :: Int -> K Double`) and use a class method (`show`) at the refined
//! type in EACH sibling alternative was reported to SIGSEGV on the JIT (the
//! "sibling-alt refined-dict" crash). This test replays the exact program
//! (fixture source: `haskell/test/SibDict.hs`) plus controls through both the
//! interpreter oracle and the Cranelift JIT and asserts byte-identical results.
//!
//! Outcome at commit 7f8dfaf: GREEN. The emit is correct — no case-trap, no
//! unfilled Con field, no dict-dispatch shape error. `progBoth` combines both
//! sibling `show`s ("5 | 1.5") and matches the oracle exactly. The original
//! crash was NOT an emit bug; its signature matches a `DataConTable`
//! `stableVarId` hash collision that silently evicted a constructor entry (the
//! freer-simple `Union` eviction class), which is table-population-sensitive
//! and does not recur with this standalone table. That silent-eviction class is
//! now caught LOUD at metadata deserialization: `read_metadata` populates via
//! `DataConTable::insert_checked` (returns `ReadError` on a genuine collision),
//! so it can no longer manifest as a SIGSEGV.
//!
//! Fixtures are captured with `tidepool-extract-bin --all-closed
//! --target-module-only` (NOINLINE seeds defeat -O2 constant folding so the
//! real per-alt dictionary Core survives to runtime). `*.cbor` is gitignored;
//! the fixtures were force-added.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::machine_state::MachineState;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_testing::compare;

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sibling_alt_dict")
}

/// Compile+run one closed-Core fixture through eval and the JIT; assert the
/// forced results are structurally equal (ByteArray content included). Returns
/// the decoded Text for a readable expectation check.
fn diff_one(name: &str) -> Option<String> {
    let dir = fixtures_dir();
    let table = read_metadata(&std::fs::read(dir.join("meta.cbor")).unwrap())
        .unwrap()
        .0;
    let env = env_from_datacon_table(&table);
    let expr = read_cbor(&std::fs::read(dir.join(format!("{name}.cbor"))).unwrap()).unwrap();

    // Interpreter oracle.
    let mut heap = VecHeap::new();
    let eval_val = eval(&expr, &env, &mut heap)
        .and_then(|v| deep_force(v, &mut heap))
        .unwrap_or_else(|e| panic!("[{name}] eval failed: {e:?}"));

    // JIT.
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, &expr, "sib_test", &ExternalEnv::new()).unwrap();
    pipeline.finalize().unwrap();

    let mut nursery = vec![0u8; 1 << 20];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    let machine_state = Box::new(MachineState::new());
    vmctx.machine_state = machine_state.as_ref() as *const MachineState as *mut MachineState;
    host_fns::set_gc_state(start, nursery.len());
    machine_state.set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result_ptr = unsafe { func(&mut vmctx as *mut VMContext) } as *const u8;
    let jit_val = unsafe {
        tidepool_codegen::heap_bridge::heap_to_value_forcing(
            result_ptr,
            &mut vmctx as *mut VMContext,
        )
    }
    .unwrap_or_else(|e| panic!("[{name}] jit bridge failed: {e:?}"));

    assert!(
        compare::values_equal(&eval_val, &jit_val),
        "[{name}] eval/jit divergence: eval={:?} jit={:?}",
        text_str(&eval_val),
        text_str(&jit_val)
    );
    text_str(&jit_val)
}

/// Decode a `Text` Con (ByteArray, offset :: Int, length :: Int) to a UTF-8
/// string. Returns None if `v` isn't Text-shaped.
fn text_str(v: &tidepool_eval::Value) -> Option<String> {
    use tidepool_eval::Value;
    use tidepool_repr::Literal;
    fn lit_usize(v: &Value) -> Option<usize> {
        match v {
            Value::Lit(Literal::LitInt(n)) => Some(*n as usize),
            _ => None,
        }
    }
    if let Value::Con(_, fields) = v {
        if let [Value::ByteArray(ba), off, len] = fields.as_slice() {
            let bytes = ba.lock().unwrap();
            let o = lit_usize(off)?;
            let l = lit_usize(len)?;
            let slice = bytes.get(o..o + l)?;
            return Some(String::from_utf8_lossy(slice).into_owned());
        }
    }
    None
}

#[test]
fn sibling_alt_refined_dict_matches_oracle() {
    // 64MB thread: complex closed Core over the interpreter can be stack-heavy.
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            // (fixture, expected Text) — the crux is `progBoth`: BOTH sibling
            // `show`s (Int and Double) elaborated per-alt in one function.
            let cases = [
                ("progInt", "5"),        // GADT KInt alt: show @Int
                ("progPrec", "1.5"),     // GADT KPrec alt: show @Double
                ("progBoth", "5 | 1.5"), // both sibling shows together (the repro)
                ("progPackInt", "5"),    // control: plain T.pack (show n), no GADT
                ("progSingle", "1.5"),   // control: single-alt GADT dict use
                ("progEitherL", "5"),    // control: Either sibling (non-GADT)
                ("progEitherR", "1.5"),  // control: Either sibling (non-GADT)
                ("progSameDict", "1.5"), // control: sibling alts, same (Double) dict
            ];
            for (name, expected) in cases {
                let got = diff_one(name);
                assert_eq!(got.as_deref(), Some(expected), "[{name}] wrong Text value");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
