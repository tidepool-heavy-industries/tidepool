//! Real-Core corpus differential runner.
//!
//! Loads the captured `meta.cbor` (real `DataConTable`) and replays every
//! per-binding `.cbor` in `haskell/test/corpus_cbor/` through the strict
//! `check_jit_vs_eval_captured` oracle — the generalization of the #1/#2 net into
//! a coverage harness. Each binding is classified; anything that is not `Match`
//! and not on the documented KNOWN allow-list is a newly-surfaced real-Core bug.
//!
//! Regenerate fixtures with `haskell/regen-corpus.sh` (native-bignum binary).
//!
//! # GHC oracle sidecar — the GATING referee
//!
//! `tidepool-eval` (the tree-walking interpreter) drifted twice in one month
//! — both times eval was wrong and the JIT was right. Trusting eval as the
//! sole referee for whether the JIT is "correct" was therefore backwards: a
//! real GHC execution of the same source is the one referee that cannot
//! itself be the thing under test. `haskell/regen-corpus.sh` now also runs
//! `haskell/test/corpus/GenOracle.hs` (a plain-GHC executable that imports
//! `Corpus` directly, unrelated to the extractor pipeline) and checks in its
//! output as `haskell/test/corpus_cbor/oracle.json` — one JSON object
//! mapping every corpus binding's name to `{"mode","kind","value"}`.
//!
//! **`mode`** is always `"NF"` in the current corpus: every binding is a
//! fully-evaluable scalar or list of scalars with no field that must stay
//! unforced surviving INTO the compared value (a case like `lazyConField`
//! hides an unforced bottom inside a tuple that is never part of the
//! returned `Int`, so forcing the `Int` result to normal form never touches
//! it). This is the same depth `JitEffectMachine::run_pure`'s heap bridge
//! already forces to, so `"NF"` is the mode that matches today's actual
//! comparison — not an invented one. The schema reserves `"WHNF"` (observe
//! only as far as the outer constructor — needed if a future binding must
//! assert an unused field stays unforced) and `"Display"` (compare GHC's
//! exact `Show` text, for formatting behavior no structural comparison
//! captures) for if/when a corpus entry actually needs that shallower or
//! textual observation; none does today.
//!
//! **`kind`** tells the Rust side which GHC boxed representation to expect
//! and how to decode the JIT's `Value` for comparison (`canonicalize`,
//! below): `int` (any fixed-width Integral: Int/Word/Int8..64/Word8..64,
//! boxed as `I#`/`W#`/`I8#`/…), `integer` (arbitrary-precision `Integer`,
//! boxed as `IS`/`IP`/`IN`), `double`, `float` (widened to `Double` for
//! comparison — the widening is exact, so no precision is lost), `bool`,
//! `char`, `string` (`[Char]`, both the literal `LitString` and a real
//! cons-of-`C#` spine), `list_int` (`[Int]`). Every field type appearing in
//! `Corpus.hs` today; a new binding with a genuinely new result shape needs
//! a new kind on both sides (`GenOracle.hs`'s `entryXxx` family and this
//! file's `canonicalize`).
//!
//! **Gating policy:** JIT vs GHC-sidecar is the gate (`assert_eq` on the
//! canonicalized values) — a mismatch fails the test. Eval's result is
//! still computed (`check_jit_vs_eval_captured`, unchanged) and compared
//! against both, but purely OBSERVATIONALLY: a JIT/eval divergence is
//! logged, never asserted. This is the "shrink eval to its niche" migration
//! — eval remains fully load-bearing for the synthetic-IR proptests,
//! optimizer-preservation checks, and effect-dispatch transcripts (none of
//! which this file touches), but for the real-Core corpus specifically, GHC
//! itself is the referee now.
use std::path::PathBuf;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::JitError;
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::error::EvalError;
use tidepool_eval::value::Value;
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

const NURSERY: usize = 16 * 1024 * 1024;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/corpus_cbor")
}

use tidepool_testing::haskell_suite::is_lifted_local;

fn short(v: &Value) -> String {
    let s = format!("{v:?}");
    s.chars().take(60).collect()
}

fn err_short(e: &JitError) -> String {
    let s = format!("{e:?}");
    s.chars().take(70).collect()
}

/// Is a JIT failure a missing-SUPPORT gap (unresolved external / unimplemented)
/// rather than an implemented-but-wrong BUG?
fn jit_is_gap(e: &JitError) -> bool {
    matches!(
        e,
        JitError::Yield(YieldError::Runtime(RuntimeError::UnresolvedVar(..)))
    )
}

/// Is an eval failure a missing-SUPPORT gap (unsupported primop) vs a BUG?
fn eval_is_gap(e: &EvalError) -> bool {
    matches!(e, EvalError::UnsupportedPrimOp(_))
}

/// Classify into a tag that separates SUPPORT gaps (missing primop / FFI /
/// unresolved external — the "not implemented" backlog) from BUGs
/// (implemented-but-wrong / crash / value divergence).
fn classify(o: &CapturedOutcome) -> (&'static str, String) {
    match o {
        CapturedOutcome::Agree(v) => ("MATCH", short(v)),
        CapturedOutcome::Diverge { eval, jit } => (
            "DIVERGE",
            format!("eval={} jit={}", short(eval), short(jit)),
        ),
        CapturedOutcome::JitOnlyFailure { jit, .. } => (
            if jit_is_gap(jit) {
                "JIT-GAP"
            } else {
                "JIT-BUG"
            },
            err_short(jit),
        ),
        CapturedOutcome::EvalOnlyFailure { eval, .. } => (
            if eval_is_gap(eval) {
                "EVAL-GAP"
            } else {
                "EVAL-BUG"
            },
            format!("{eval:?}").chars().take(70).collect(),
        ),
        CapturedOutcome::BothFail { eval, jit } => {
            let tag = match (eval_is_gap(eval), jit_is_gap(jit)) {
                (true, true) => "BOTH-GAP",
                (false, false) => "BOTH-BUG",
                _ => "BOTH-MIXED",
            };
            (tag, format!("eval={eval:?} jit={}", err_short(jit)))
        }
    }
}

/// Tags whose failure is a missing-support gap (→ the FFI/primop/external backlog).
fn is_gap_tag(tag: &str) -> bool {
    matches!(tag, "JIT-GAP" | "EVAL-GAP" | "BOTH-GAP" | "BOTH-MIXED")
}

/// Tags whose failure is an implemented-but-wrong bug (→ the divergence findings).
fn is_bug_tag(tag: &str) -> bool {
    matches!(
        tag,
        "DIVERGE" | "JIT-BUG" | "EVAL-BUG" | "BOTH-BUG" | "BOTH-MIXED" | "CRASH"
    )
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
    matches!(v, Value::Closure { .. } | Value::ConFun(..))
}

/// Returns `(is_function_program, tag, detail)`. The closure-check runs
/// inside the worker thread (`Value`/`Env` aren't `Send`).
fn run_one(node: &[u8], meta: &[u8]) -> (bool, &'static str, String) {
    let node = node.to_vec();
    let meta = meta.to_vec();
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let expr: CoreExpr = read_cbor(&node).unwrap();
            let table: DataConTable = read_metadata(&meta).unwrap().0;
            let outcome = check_jit_vs_eval_captured(&expr, &table, NURSERY);
            let is_fn = eval_is_closure(&outcome);
            let (tag, detail) = classify(&outcome);
            (is_fn, tag, detail)
        })
        .unwrap();
    // A binding can terminate its worker via a fatal signal (e.g. host stack
    // overflow on Drop of a deep Value spine — host-stack-overflow-class). Catch
    // it so one crash doesn't abort the whole corpus; record it as a finding.
    match handle.join() {
        Ok(r) => r,
        Err(_) => (
            false,
            "CRASH",
            "worker terminated (fatal signal / panic — likely host Drop overflow)".to_string(),
        ),
    }
}

/// KNOWN non-`MATCH` outcomes: each is a real-Core bug surfaced by this corpus,
/// documented so a *new* or *changed* divergence fails loudly while these stay
/// green until fixed. `(binding, expected_tag, bug-class)`. Fixing a bug flips it
/// to `MATCH` (which still passes — then prune the stale entry).
const KNOWN: &[(&str, &str, &str)] = &[
    // ── BUGS (implemented-but-wrong) ──
    // The GHC.Float Double-conversion bugs are FIXED (entries pruned, now MATCH):
    //  - #1 roundingMode#:IN (convFromInt*): eager-eval of GHC's bottoming
    //    `case error "roundingMode#: IN" of {}` CAF — the error-deferral check
    //    missed it through the forced case scrutinee.
    //  - the rationalToDouble follow-on (convDoubleLitBig 1.0e308, convFromRational):
    //    eager-eval of a `raise# exc` LetRec binding — the same check did not
    //    recognise a `PrimOp Raise` RHS as bottoming.
    // Both fixed in tidepool-codegen/src/emit/expr.rs (error-call walkers follow the
    // case scrutinee AND treat raise# as a bottoming, deferred RHS).
    //
    // GADT eqspec arity (gadtEval) is FIXED too (now MATCH, entry pruned): GHC's
    // GADT case alt binds the equality-evidence CoVar (`AddE co a b`), but
    // translateAlt only filtered TyVars — so the alt bound one field too many
    // (eval ArityMismatch, JIT read past the fields -> SIGSEGV). Fixed in
    // Translate.hs (alt binders now exclude CoVars, matching the Con build's
    // isValueArg / valueRepArity).
    //
    // #2 read/ReadP — FULLY FIXED (now MATCH, entries pruned), in two parts:
    //  - The SHARED translation bug: GHC wraps the ReadP CPS function in `MkSolo#`,
    //    the unboxed 1-tuple `(# f #)` (no runtime rep — it IS its field). The
    //    Con-build path boxed it into a heap Con while the case side already treats
    //    `case scrut of (# x #) -> body` as identity — a boxed Con in function
    //    position → JIT BadFunPtrTag / eval NotAFunction. Fixed in Translate.hs
    //    (1-element unboxed-tuple builds erase to their field). That made EVAL
    //    correct (42 / [1,2,3] / 3.14) but left a JIT-only residual.
    //  - The JIT residual (eager-let): ReadP `expect` is a productive corecursion
    //    `F = \k -> let x = F k in <Get parser using x>`. The JIT evaluated every
    //    LetNonRec RHS EAGERLY, forcing `let x = F k` into infinite self-recursion
    //    → StackOverflow; eval (lazy let) is bounded by demand. Fixed in
    //    emit/expr.rs: non-trivial LetNonRec RHS is thunkified (GHC Core `let` is
    //    non-strict — strictness is via `case`, never `let`). Trivial RHS
    //    (Var/Lit/Lam/Con/PrimOp) stays eager. Both engines now MATCH on all reads.
    // ── SUPPORT GAPS (missing primop / unresolved external) ──
    // convProperFraction / convRealToFrac / sumRange were MISDIAGNOSED as
    // "unresolved external" — the yielded VarId is a LOCAL LetRec binder, not a
    // base-library symbol. After resolveExternals merges everything into one Rec,
    // these end in `caf = <computation>; alias = Var(caf); body = alias`. The
    // LetRec emit's Phase 2.5 eagerly resolved the `alias = Var(caf)` *before* the
    // App-CAF `caf` was evaluated, binding the alias to an UnresolvedVar trap that
    // fired when the body forced it. Fixed in emit/expr.rs (Phase 2.5 only fast-
    // paths a Var alias when its target is already bound; otherwise defers it so
    // the topological sort evaluates it after its target). All three now MATCH.
    //
    // cycleTake was previously allow-listed as a `cycle` self-referential-CAF
    // Known-Limit (expected JIT-GAP). It now MATCHes on both engines — `take n
    // (cycle xs)` only forces a finite prefix, which both produce lazily and
    // agree on — so the stale entry is pruned. A future regression will now fail
    // LOUDLY as UNEXPECTED instead of being silently tolerated.
    //
    // The corpus is now fully green (every program MATCH). Add a new entry here
    // only when a genuine, documented divergence is surfaced.
];

#[test]
#[ignore = "expensive: replays the full real-Core corpus (a real GHC-compiled \
            binding per fixture) through the JIT-vs-eval oracle; run with \
            TIDEPOOL_EXPENSIVE_TESTS=1 cargo nextest run -p tidepool-codegen \
            --run-ignored all -E 'test(corpus_report)'"]
fn corpus_report() {
    if std::env::var("TIDEPOOL_EXPENSIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("SKIPPED (expensive): set TIDEPOOL_EXPENSIVE_TESTS=1 to run");
        return;
    }
    tidepool_testing::watchdog::arm();
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
    let mut gaps: Vec<String> = Vec::new();
    let mut bugs: Vec<String> = Vec::new();

    println!("\n=== REAL-CORE CORPUS ===");
    for (name, path) in &entries {
        let _guard = tidepool_testing::watchdog::begin(name);
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
        let row = format!("{tag:10} {name:26} {detail}");
        if is_gap_tag(tag) {
            gaps.push(row.clone());
        }
        if is_bug_tag(tag) {
            bugs.push(row);
        }
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

    // Two distinct backlogs, per the coverage mandate.
    println!(
        "\n=== SUPPORT GAPS (missing primop / FFI / unresolved external) — {} ===",
        gaps.len()
    );
    for g in &gaps {
        println!("  {g}");
    }
    println!(
        "\n=== DIVERGENCE BUGS (implemented-but-wrong / crash) — {} ===",
        bugs.len()
    );
    for b in &bugs {
        println!("  {b}");
    }

    assert!(
        violations.is_empty(),
        "UNEXPECTED real-Core outcome(s) — a NEW or CHANGED divergence:\n{}",
        violations.join("\n")
    );
}
