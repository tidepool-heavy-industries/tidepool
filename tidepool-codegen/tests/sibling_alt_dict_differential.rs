//! Ledger #39(a) regression guard: sibling-alt refined-dictionary methods.
//!
//! A GADT whose alternatives refine the type index (`KInt :: K Int`,
//! `KPrec :: Int -> K Double`) and use a class method (`render`) at the refined
//! type in EACH sibling alternative was reported to SIGSEGV on the JIT (the
//! "sibling-alt refined-dict" crash). This test replays the exact program
//! (fixture source: `haskell/test/SibDict.hs`) plus controls through both the
//! interpreter oracle and the Cranelift JIT and asserts byte-identical results.
//!
//! Outcome at commit 7f8dfaf: GREEN. The emit is correct — no case-trap, no
//! unfilled Con field, no dict-dispatch shape error. `progBoth` combines both
//! sibling `render`s ("5 | 1.5") and matches the oracle exactly. The original
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

use tidepool_codegen::jit_machine::JitEffectMachine;
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

    // JIT through the production entry point, which installs the constructor
    // registries required by managed-value host functions.
    let jit_val = JitEffectMachine::compile(&expr, &table, 1 << 20)
        .and_then(|mut machine| machine.run_pure())
        .unwrap_or_else(|e| panic!("[{name}] JIT failed: {e:?}"));

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
            // `render`s (Int and Double) elaborated per-alt in one function.
            let cases = [
                ("progInt", "5"),        // GADT KInt alt: render @Int
                ("progPrec", "1.5"),     // GADT KPrec alt: render @Double
                ("progBoth", "5 | 1.5"), // both sibling renders together (the repro)
                ("progPackInt", "5"),    // control: plain render, no GADT
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
