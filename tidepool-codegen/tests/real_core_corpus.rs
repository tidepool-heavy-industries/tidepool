//! Real-Core corpus differential runner.
//!
//! Loads the captured `meta.cbor` (real `DataConTable`) and replays every
//! per-binding `.cbor` in `haskell/test/corpus_cbor/` through the strict
//! `check_jit_vs_eval_captured` oracle — the generalization of the #1/#2 net into
//! a coverage harness. Each binding is classified; anything that is not `Match`
//! and not on the documented KNOWN allow-list is a newly-surfaced real-Core bug.
//!
//! Regenerate fixtures with `haskell/regen-corpus.sh` (native-bignum binary).
use std::collections::BTreeSet;
use std::path::PathBuf;
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

/// Is a JIT failure a missing-SUPPORT gap (unresolved external / unimplemented)
/// rather than an implemented-but-wrong BUG?
fn jit_is_gap(e: &JitError) -> bool {
    matches!(e, JitError::Yield(YieldError::UnresolvedVar(_)))
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
    matches!(v, Value::Closure(..) | Value::ConFun(..))
}

/// Returns `(is_function_program, tag, detail, emit_coverage)`. The closure-check
/// and the emit-coverage snapshot both run inside the worker thread (`Value`/`Env`
/// aren't `Send`, and coverage is a per-thread set populated during this compile).
fn run_one(node: &[u8], meta: &[u8]) -> (bool, &'static str, String, BTreeSet<&'static str>) {
    let node = node.to_vec();
    let meta = meta.to_vec();
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let expr: CoreExpr = read_cbor(&node).unwrap();
            let table: DataConTable = read_metadata(&meta).unwrap().0;
            tidepool_codegen::coverage::reset();
            let outcome = check_jit_vs_eval_captured(&expr, &table, NURSERY);
            let cov = tidepool_codegen::coverage::snapshot();
            let is_fn = eval_is_closure(&outcome);
            let (tag, detail) = classify(&outcome);
            (is_fn, tag, detail, cov)
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
            BTreeSet::new(),
        ),
    }
}

/// KNOWN non-`MATCH` outcomes: each is a real-Core bug surfaced by this corpus,
/// documented so a *new* or *changed* divergence fails loudly while these stay
/// green until fixed. `(binding, expected_tag, bug-class)`. Fixing a bug flips it
/// to `MATCH` (which still passes — then prune the stale entry).
const KNOWN: &[(&str, &str, &str)] = &[
    // ── BUGS (implemented-but-wrong) ──
    // #1 (roundingMode#:IN) FIXED — the convFromInt* fromIntegral cases now MATCH.
    // Root cause was eager-eval of GHC's bottoming `case error "roundingMode#: IN"
    // of {}` CAF (the error-deferral check missed it through the forced case
    // scrutinee); fix in tidepool-codegen/src/emit/expr.rs. Entries pruned.
    //
    // FOLLOW-ON (un-masked by the #1 fix, same way PopCnt un-masked #1): the
    // Double-literal / Rational->Double path (rationalToDouble) no longer raises
    // roundingMode# but now raises a bare UserError — the JIT force-evaluates a
    // (correctly-poisoned) error binding on the live path where eval defers it
    // (eager-force class, distinct from the eager-CAF-eval #1 just fixed; eval
    // returns the exact value). Tracked as a separate root-cause.
    (
        "convDoubleLitBig",
        "JIT-BUG",
        "FOLLOW-ON: rationalToDouble JIT eager-forces a poison (UserError); eval ok",
    ),
    (
        "convFromRational",
        "JIT-BUG",
        "FOLLOW-ON: rationalToDouble JIT eager-forces a poison (UserError); eval ok",
    ),
    // GADT with equality evidence — eval case-binder arity off, JIT SIGSEGV.
    (
        "gadtEval",
        "BOTH-BUG",
        "GADT eqspec arity (eval) + SIGSEGV (JIT)",
    ),
    // #2 — ReadP ~R# newtype coercion: a Con in function position. Both engines.
    ("readInt", "BOTH-BUG", "#2 read/ReadP newtype-coercion"),
    ("readListInt", "BOTH-BUG", "#2 read/ReadP newtype-coercion"),
    ("readDouble", "BOTH-BUG", "#2 read + #1 (JIT roundingMode#)"),
    // ── SUPPORT GAPS (missing primop / unresolved external) ──
    (
        "convProperFraction",
        "JIT-GAP",
        "properFraction unresolved-external",
    ),
    (
        "convRealToFrac",
        "JIT-GAP",
        "realToFrac unresolved-external",
    ),
    (
        "sumRange",
        "JIT-GAP",
        "sum [1..n] unresolved-external (eval=5050 ok)",
    ),
    (
        "cycleTake",
        "JIT-GAP",
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
    let mut coverage: BTreeSet<&'static str> = BTreeSet::new();
    let mut gaps: Vec<String> = Vec::new();
    let mut bugs: Vec<String> = Vec::new();

    println!("\n=== REAL-CORE CORPUS ===");
    for (name, path) in &entries {
        let node = std::fs::read(path).unwrap();
        let (is_fn, tag, detail, cov) = run_one(&node, &meta);
        coverage.extend(cov);
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

    // Emit-path coverage (P2): which emitter decision points the corpus exercised.
    // Two dimensions: STRUCTURAL (frame/case/con shapes) and PRIMOP (per opcode).
    if tidepool_codegen::coverage::is_enabled() {
        println!("\n=== EMIT-PATH COVERAGE ===");

        let s_tgt = tidepool_codegen::coverage::TARGETS;
        let s_unhit: Vec<&&str> = s_tgt.iter().filter(|t| !coverage.contains(*t)).collect();
        println!(
            "  structural: {}/{} ({:.0}%) hit",
            s_tgt.len() - s_unhit.len(),
            s_tgt.len(),
            100.0 * (s_tgt.len() - s_unhit.len()) as f64 / s_tgt.len() as f64
        );
        if !s_unhit.is_empty() {
            println!("    UNHIT: {s_unhit:?}");
        }

        let primops: Vec<&'static str> = tidepool_repr::PrimOpKind::ALL_VARIANTS
            .iter()
            .map(|p| p.serial_name())
            .collect();
        let p_unhit: Vec<&&str> = primops.iter().filter(|p| !coverage.contains(*p)).collect();
        println!(
            "  primops:    {}/{} ({:.0}%) hit",
            primops.len() - p_unhit.len(),
            primops.len(),
            100.0 * (primops.len() - p_unhit.len()) as f64 / primops.len() as f64
        );
        let p_hit: Vec<&&str> = primops.iter().filter(|p| coverage.contains(*p)).collect();
        println!("    HIT primops ({}): {p_hit:?}", p_hit.len());
        println!("    UNHIT primops ({}) — residual reach:", p_unhit.len());
        println!("    {p_unhit:?}");
        // The residual is dominated by opcodes UNREACHABLE from surface Haskell —
        // a generator emitting raw Core could hit them; curation cannot:
        //  - GHC rewrites the surface op away: `x - c` -> `x + negate c`
        //    (DoubleSub/FloatSub), `x /= y` -> `not (x == y)` (DoubleNe/CharNe),
        //    `x >= y` -> `not (x < y)` (DoubleGe). Only the rewritten-TO op is hit.
        //  - 64-bit representation collapse: Int64*/Word64* == Int*/Word* on a
        //    64-bit host, so surface Int64/Word64 arithmetic emits the Int/Word op.
        //  - narrowing via mask: `fromIntegral :: Word8` emits `and# 0xFF`, not
        //    narrow8Word#.
        //  - compiler-internal: TagToEnum / SeqOp desugar to `case`; Raise /
        //    ReallyUnsafePtrEquality are not surfaced.
        //  - eval-oracle GAP (separate task): boxed Array#/SmallArray# ops need a
        //    boxed-array `Value` variant the tree-walker lacks.
        //  - need Data.Text / ByteString: FfiStrlen / FfiText* / low-level
        //    ByteArray ops (ReadWord8Array, SetByteArray, ...).
        println!(
            "    (residual ≈ GHC-rewritten-away + 64-bit-collapsed + eval-array-gap\n     + Text/ByteArray + compiler-internal — see source note; generator-only.)"
        );
    } else {
        println!("\n(emit-path coverage off — set TIDEPOOL_EMIT_COVERAGE=1)");
    }

    assert!(
        violations.is_empty(),
        "UNEXPECTED real-Core outcome(s) — a NEW or CHANGED divergence:\n{}",
        violations.join("\n")
    );
}
