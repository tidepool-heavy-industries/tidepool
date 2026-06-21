//! Real-Core corpus differential runner.
//!
//! Loads the captured `meta.cbor` (real `DataConTable`) and replays every
//! per-binding `.cbor` in `haskell/test/corpus_cbor/` through the strict
//! `check_jit_vs_eval_captured` oracle — the generalization of the #1/#2 net into
//! a coverage harness. Each binding is classified; anything that is not `Match`
//! and not on the documented KNOWN allow-list is a newly-surfaced real-Core bug.
//!
//! Regenerate fixtures with `haskell/regen-corpus.sh` (native-bignum binary).
use std::path::PathBuf;
use tidepool_codegen::jit_machine::JitError;
use tidepool_eval::value::Value;
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

const NURSERY: usize = 16 * 1024 * 1024;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/corpus_cbor")
}

/// GHC lifts `where`/`let` helpers to uniquified top-level binders
/// (`go_u6341068275337658369`). Those are inlined into the bindings that use
/// them; running them standalone tests nothing. Skip them.
fn is_lifted_local(name: &str) -> bool {
    match name.rfind("_u") {
        Some(i) => {
            let tail = &name[i + 2..];
            tail.len() >= 6 && tail.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn short(v: &Value) -> String {
    let s = format!("{v:?}");
    s.chars().take(60).collect()
}

fn err_short(e: &JitError) -> String {
    let s = format!("{e:?}");
    s.chars().take(70).collect()
}

/// (tag, detail) for the table.
fn classify(o: &CapturedOutcome) -> (&'static str, String) {
    match o {
        CapturedOutcome::Agree(v) => ("MATCH", short(v)),
        CapturedOutcome::Diverge { eval, jit } => (
            "DIVERGE",
            format!("eval={} jit={}", short(eval), short(jit)),
        ),
        CapturedOutcome::JitOnlyFailure { jit, .. } => ("JIT-ERR", err_short(jit)),
        CapturedOutcome::EvalOnlyFailure { eval, .. } => {
            ("EVAL-ERR", format!("{eval:?}").chars().take(70).collect())
        }
        CapturedOutcome::BothFail { eval, jit } => (
            "BOTH-ERR",
            format!("eval={:?} jit={}", eval, err_short(jit)),
        ),
    }
}

/// A helper function (GHC lifts `where`/instance methods / record selectors to
/// top level) evaluates to a `Closure` — running it standalone isn't a meaningful
/// "program" (the JIT can't bridge a closure as a pure result). Detected from the
/// eval result so it survives the closed-Core `LetRec{body=Var}` wrapper that
/// hides the root `Lam`.
fn eval_is_closure(o: &CapturedOutcome) -> bool {
    let v = match o {
        CapturedOutcome::Agree(v) => v,
        CapturedOutcome::JitOnlyFailure { eval, .. } => eval,
        CapturedOutcome::Diverge { eval, .. } => eval,
        _ => return false,
    };
    matches!(v, Value::Closure(..) | Value::ConFun(..))
}

/// Returns `(is_function_program, tag, detail)`. The closure-check runs inside the
/// worker thread because `Value`/`Env` are not `Send`.
fn run_one(node: &[u8], meta: &[u8]) -> (bool, &'static str, String) {
    let node = node.to_vec();
    let meta = meta.to_vec();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let expr: CoreExpr = read_cbor(&node).unwrap();
            let table: DataConTable = read_metadata(&meta).unwrap().0;
            let outcome = check_jit_vs_eval_captured(&expr, &table, NURSERY);
            let is_fn = eval_is_closure(&outcome);
            let (tag, detail) = classify(&outcome);
            (is_fn, tag, detail)
        })
        .unwrap()
        .join()
        .unwrap()
}

/// KNOWN non-`MATCH` outcomes: each is a real-Core bug surfaced by this corpus,
/// documented so a *new* or *changed* divergence fails loudly while these stay
/// green until fixed. `(binding, expected_tag, bug-class)`. Fixing a bug flips it
/// to `MATCH` (which still passes — then prune the stale entry).
const KNOWN: &[(&str, &str, &str)] = &[
    // #1 — JIT mis-dispatches the inlined roundingMode# Integer case (IS->IN).
    // ALL non-folded fromIntegral Integer->Double, magnitude-independent.
    ("convFromInt5", "JIT-ERR", "#1 roundingMode#:IN"),
    ("convFromInt1025", "JIT-ERR", "#1 roundingMode#:IN"),
    ("convFromIntPow40", "JIT-ERR", "#1 roundingMode#:IN"),
    ("convFromIntPow80", "JIT-ERR", "#1 roundingMode#:IN"),
    // #1 reached via Rational/Double-literal, AND eval lacks the PopCnt primop.
    (
        "convDoubleLitBig",
        "BOTH-ERR",
        "#1 (JIT) + eval-missing-PopCnt",
    ),
    (
        "convFromRational",
        "BOTH-ERR",
        "#1 (JIT) + eval-missing-PopCnt",
    ),
    // NEW: JIT leaves a GHC.Float/Real external unresolved on these paths.
    (
        "convProperFraction",
        "JIT-ERR",
        "NEW: properFraction unresolved-external (JIT)",
    ),
    (
        "convRealToFrac",
        "JIT-ERR",
        "NEW: realToFrac unresolved-external (JIT)",
    ),
    (
        "sumRange",
        "JIT-ERR",
        "NEW: sum [1..n] unresolved-external (JIT; eval=5050 ok)",
    ),
    // NEW: GADT with equality evidence — eval case-binder arity off, JIT SIGSEGV.
    (
        "gadtEval",
        "BOTH-ERR",
        "NEW: GADT eqspec arity (eval) + SIGSEGV (JIT)",
    ),
    // #2 — ReadP ~R# newtype coercion: a Con in function position. Both engines.
    ("readInt", "BOTH-ERR", "#2 read/ReadP newtype-coercion"),
    ("readListInt", "BOTH-ERR", "#2 read/ReadP newtype-coercion"),
    ("readDouble", "BOTH-ERR", "#2 read + #1 (JIT roundingMode#)"),
    // Known-Limit: cycle is an unresolved external by design.
    (
        "cycleTake",
        "JIT-ERR",
        "Known-Limit: cycle unresolved-external",
    ),
];

#[test]
fn corpus_report() {
    let dir = corpus_dir();
    let meta = std::fs::read(dir.join("meta.cbor")).expect("meta.cbor — run regen-corpus.sh");

    let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "cbor").unwrap_or(false))
        .filter_map(|p| {
            let name = p.file_stem()?.to_str()?.to_string();
            if name == "meta" || is_lifted_local(&name) {
                return None;
            }
            Some((name, p))
        })
        .collect();
    entries.sort();

    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut funcs = 0usize;
    let mut violations: Vec<String> = Vec::new();

    println!("\n=== REAL-CORE CORPUS ===");
    for (name, path) in &entries {
        let node = std::fs::read(path).unwrap();
        let (is_fn, tag, detail) = run_one(&node, &meta);
        if is_fn {
            funcs += 1; // helper function (Closure result) — not a program; skip.
            continue;
        }
        *counts.entry(tag).or_insert(0) += 1;
        let known = KNOWN.iter().find(|(n, ..)| n == name);
        let mark = match (tag, known) {
            ("MATCH", _) => "ok",
            (_, Some((_, exp, _))) if *exp == tag => "known",
            _ => "** UNEXPECTED **",
        };
        println!("{tag:9} {name:26} {mark:18} {detail}");
        if mark == "** UNEXPECTED **" {
            violations.push(format!("{tag:9} {name:26} {detail}"));
        }
    }

    println!(
        "\n=== SUMMARY ({} programs, {} helper fns skipped) ===",
        counts.values().sum::<usize>(),
        funcs
    );
    for (tag, n) in &counts {
        println!("  {tag:9} {n}");
    }
    println!("\n=== KNOWN real-Core bugs surfaced ===");
    for (n, tag, note) in KNOWN {
        println!("  {tag:9} {n:26} {note}");
    }

    assert!(
        violations.is_empty(),
        "UNEXPECTED real-Core outcome(s) — a NEW or CHANGED divergence:\n{}",
        violations.join("\n")
    );
}
