//! Real-Core corpus differential runner.
//!
//! Loads the captured `meta.cbor` (real `DataConTable`) and replays every
//! per-binding `.cbor` in `haskell/test/corpus_cbor/` — running the JIT and
//! eval independently and classifying each binding against the checked-in
//! GHC oracle sidecar (see below). Anything that is not `MATCH` and not on
//! the documented KNOWN allow-list is a newly-surfaced real-Core bug.
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
//! and how to decode the JIT's `Value` for comparison (`value_to_canon`,
//! below): `int` (any fixed-width Integral: Int/Word/Int8..64/Word8..64,
//! boxed as `I#`/`W#`/`I8#`/…), `integer` (arbitrary-precision `Integer`,
//! boxed as `IS`/`IP`/`IN`), `double`, `float` (widened to `Double` for
//! comparison — the widening is exact, so no precision is lost), `bool`,
//! `char`, `string` (`[Char]`, both the literal `LitString` and a real
//! cons-of-`C#` spine), `list_int` (`[Int]`). Every field type appearing in
//! `Corpus.hs` today; a new binding with a genuinely new result shape needs
//! a new kind on both sides (`GenOracle.hs`'s `entryXxx` family and this
//! file's `value_to_canon`).
//!
//! **Gating policy:** JIT vs GHC-sidecar is the gate (`assert_eq`-equivalent
//! on the canonicalized values) — a mismatch fails the test. Both engines
//! are run INDEPENDENTLY here (`eval` and `JitEffectMachine::run_pure`
//! directly, not through `check_jit_vs_eval_captured`/`CapturedOutcome`):
//! that shared helper's lenient `values_equal` treats an unforced `ThunkRef`
//! on EITHER side as "incomparable, skip" (correct for its own synthetic-IR
//! use, documented on that `values_equal`), and its `Agree` variant then
//! keeps only the EVAL value — discarding the real, independently-forced
//! JIT value even when eval's is legitimately WHNF-only (e.g. a lazy list
//! spine). Gating against GHC's fully-forced NF sidecar needs that real JIT
//! value, so this file computes it directly. Eval's result is still
//! computed and compared against both, but purely OBSERVATIONALLY: a
//! JIT/eval divergence is logged, never asserted. This is the "shrink eval
//! to its niche" migration — eval remains fully load-bearing for the
//! synthetic-IR proptests, optimizer-preservation checks, and
//! effect-dispatch transcripts (none of which this file touches), but for
//! the real-Core corpus specifically, GHC itself is the referee now.
use serde_json::Value as Json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::error::EvalError;
use tidepool_eval::shapes;
use tidepool_eval::value::Value;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::serial::read::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable, Literal};

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

/// Tags whose failure is a missing-support gap (→ the FFI/primop/external backlog).
fn is_gap_tag(tag: &str) -> bool {
    tag == "JIT-GAP"
}

/// Tags whose failure is an implemented-but-wrong bug (→ the divergence findings).
fn is_bug_tag(tag: &str) -> bool {
    matches!(tag, "DIVERGE" | "JIT-BUG" | "CRASH" | "NO-ORACLE")
}

/// A helper function (GHC lifts `where`/instance methods / record selectors to
/// top level) evaluates to a `Closure` — running it standalone isn't a meaningful
/// "program" (the JIT can't bridge a closure as a pure result). Detected from the
/// eval result so it survives the closed-Core `LetRec{body=Var}` wrapper that
/// hides the root `Lam`.
fn eval_is_closure(eval_result: &Result<Value, EvalError>) -> bool {
    matches!(
        eval_result,
        Ok(Value::Closure { .. }) | Ok(Value::ConFun(..))
    )
}

// ---------------------------------------------------------------------------
// The GHC oracle sidecar: parsing + the canonical value both sides decode to.
// ---------------------------------------------------------------------------

/// One oracle.json entry: `(kind, value)` — `mode` is dropped after loading
/// since every entry today is `"NF"` (see the module doc); nothing branches
/// on it yet.
type OracleEntry = (String, Json);

fn load_oracle(dir: &Path) -> HashMap<String, OracleEntry> {
    let raw = std::fs::read_to_string(dir.join("oracle.json"))
        .expect("oracle.json — run regen-corpus.sh (GHC oracle sidecar generation)");
    let parsed: Json =
        serde_json::from_str(&raw).expect("oracle.json must be valid JSON (regen-corpus.sh bug)");
    let obj = parsed
        .as_object()
        .expect("oracle.json must be a JSON object");
    obj.iter()
        .map(|(name, entry)| {
            let kind = entry
                .get("kind")
                .and_then(Json::as_str)
                .unwrap_or_else(|| panic!("oracle.json entry {name:?} missing \"kind\""))
                .to_string();
            let value = entry
                .get("value")
                .unwrap_or_else(|| panic!("oracle.json entry {name:?} missing \"value\""))
                .clone();
            (name.clone(), (kind, value))
        })
        .collect()
}

/// The canonical value shape both the GHC oracle sidecar and the JIT/eval
/// `Value` decode into for comparison. Deliberately NOT `serde_json::Value`
/// equality: `serde_json::Number` distinguishes an integer-typed number from
/// a float-typed number holding the same magnitude (`Number(4)` from a JSON
/// integer literal vs `Number(4.0)` from a float literal are different enum
/// variants internally), so leaning on it here would risk a false DIVERGE
/// between, say, an oracle value written as `4` and a decoded `4.0` that are
/// actually equal. Typed comparison sidesteps that entirely.
#[derive(Debug, Clone, PartialEq)]
enum Canon {
    /// Any fixed-width Integral (Int/Word/Int8..64/Word8..64).
    Int(i128),
    /// Arbitrary-precision `Integer` (GHC's IS/IP/IN), as a normalized
    /// decimal string — never parsed into a fixed-width type, since a
    /// genuinely huge future corpus entry must not silently overflow.
    Integer(String),
    /// `Double` and `Float` (widened — the widening is exact) share one
    /// variant: comparison policy is identical once both are `f64`.
    Double(f64),
    Bool(bool),
    Char(char),
    /// `String` ([Char]).
    Str(String),
    /// `[Int]`.
    IntList(Vec<i128>),
}

fn canon_mismatch(kind: &str, what: &str, detail: impl std::fmt::Debug) -> String {
    format!("expected {kind}-shaped value, got {what}: {detail:?}")
}

/// Decode an oracle.json `(kind, value)` pair into `Canon`.
fn oracle_to_canon(kind: &str, value: &Json) -> Result<Canon, String> {
    match kind {
        "int" => value
            .as_str()
            .and_then(|s| s.parse::<i128>().ok())
            .map(Canon::Int)
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        "integer" => value
            .as_str()
            .map(|s| Canon::Integer(s.to_string()))
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        "double" | "float" => value
            .as_f64()
            .map(Canon::Double)
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        "bool" => value
            .as_bool()
            .map(Canon::Bool)
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        "char" => value
            .as_str()
            .and_then(|s| {
                let mut cs = s.chars();
                let c = cs.next()?;
                cs.next().is_none().then_some(c)
            })
            .map(Canon::Char)
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        "string" => value
            .as_str()
            .map(|s| Canon::Str(s.to_string()))
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        "list_int" => value
            .as_array()
            .and_then(|a| a.iter().map(|x| x.as_i64().map(|n| n as i128)).collect())
            .map(Canon::IntList)
            .ok_or_else(|| canon_mismatch(kind, "oracle value", value)),
        other => Err(format!("unknown oracle kind {other:?}")),
    }
}

fn con_name<'a>(v: &Value, table: &'a DataConTable) -> Option<&'a str> {
    match v {
        Value::Con(id, _) => table.name_of(*id),
        _ => None,
    }
}

/// Any known fixed-width boxing constructor: `I#`/`W#` plus the narrow
/// `Int8#`-family (`Data.Int`/`Data.Word` newtypes over a machine-width
/// literal) — every one of `Corpus.hs`'s `kind = "int"` result types
/// (`Int`, `Word`, `Int64`, `Word8`, …) boxes through one of these.
const FIXED_INT_CONS: &[&str] = &["I#", "I8#", "I16#", "I32#", "I64#"];
const FIXED_WORD_CONS: &[&str] = &["W#", "W8#", "W16#", "W32#", "W64#"];

fn unbox_any_fixed_int(v: &Value, table: &DataConTable) -> Option<i128> {
    let Value::Con(id, fields) = v else {
        return None;
    };
    if fields.len() != 1 {
        return None;
    }
    let name = table.name_of(*id)?;
    match &fields[0] {
        Value::Lit(Literal::LitInt(n)) if FIXED_INT_CONS.contains(&name) => Some(*n as i128),
        Value::Lit(Literal::LitWord(n)) if FIXED_WORD_CONS.contains(&name) => Some(*n as i128),
        _ => None,
    }
}

/// Decode a GHC `Integer` (`IS`/`IP`/`IN`) into a normalized decimal string,
/// reusing `tidepool_eval::shapes`'s bignat-limb decoder rather than
/// hand-rolling one — see that module's doc: "the ONE home for how common
/// GHC runtime shapes look as `Value` trees."
fn unbox_integer(v: &Value, table: &DataConTable) -> Option<String> {
    let Value::Con(id, fields) = v else {
        return None;
    };
    match (table.name_of(*id)?, fields.as_slice()) {
        ("IS", [x]) => match x {
            Value::Lit(Literal::LitInt(n)) => Some(n.to_string()),
            _ => shapes::unbox_int(x, table).map(|n| n.to_string()),
        },
        ("IP", [x]) => {
            shapes::bignat_backing_bytes(x, table).map(|b| shapes::bignat_bytes_to_decimal(&b))
        }
        ("IN", [x]) => shapes::bignat_backing_bytes(x, table)
            .map(|b| format!("-{}", shapes::bignat_bytes_to_decimal(&b))),
        _ => None,
    }
}

/// Decode a `String` ([Char]): either the literal `LitString` GHC folds a
/// constant string into, or a real cons-of-boxed-`C#` spine built at runtime
/// (e.g. `show`'s output).
fn jit_string(v: &Value, table: &DataConTable) -> Option<String> {
    if let Value::Lit(Literal::LitString(bytes)) = v {
        return String::from_utf8(bytes.clone()).ok();
    }
    let mut s = String::new();
    let mut cur = v;
    loop {
        match cur {
            Value::Lit(Literal::LitString(bytes)) => {
                s.push_str(std::str::from_utf8(bytes).ok()?);
                return Some(s);
            }
            _ => match (con_name(cur, table), cur) {
                (Some("[]"), Value::Con(_, fields)) if fields.is_empty() => return Some(s),
                (Some(":"), Value::Con(_, fields)) if fields.len() == 2 => {
                    s.push(shapes::unbox_char(&fields[0], table)?);
                    cur = &fields[1];
                }
                _ => return None,
            },
        }
    }
}

/// Decode a `[Int]` cons spine into a `Vec<i128>`.
fn jit_int_list(v: &Value, table: &DataConTable) -> Option<Vec<i128>> {
    let mut out = Vec::new();
    let mut cur = v;
    loop {
        match (con_name(cur, table), cur) {
            (Some("[]"), Value::Con(_, fields)) if fields.is_empty() => return Some(out),
            (Some(":"), Value::Con(_, fields)) if fields.len() == 2 => {
                out.push(unbox_any_fixed_int(&fields[0], table)?);
                cur = &fields[1];
            }
            _ => return None,
        }
    }
}

/// Decode a JIT-or-eval `Value` into `Canon`, dispatched by the oracle's
/// recorded `kind`. Shared by the gating (JIT) and observational (eval)
/// comparisons — both engines produce the same `tidepool_eval::Value` shape.
fn value_to_canon(kind: &str, v: &Value, table: &DataConTable) -> Result<Canon, String> {
    match kind {
        "int" => unbox_any_fixed_int(v, table)
            .map(Canon::Int)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "integer" => unbox_integer(v, table)
            .map(Canon::Integer)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "double" => shapes::unbox_double(v, table)
            .map(Canon::Double)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "float" => shapes::unbox_float(v, table)
            .map(|f| Canon::Double(f as f64))
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "bool" => shapes::unbox_bool(v, table)
            .map(Canon::Bool)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "char" => shapes::unbox_char(v, table)
            .map(Canon::Char)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "string" => jit_string(v, table)
            .map(Canon::Str)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        "list_int" => jit_int_list(v, table)
            .map(Canon::IntList)
            .ok_or_else(|| canon_mismatch(kind, "Value", v)),
        other => Err(format!("unknown oracle kind {other:?}")),
    }
}

/// Result of one binding's replay: `(is_function_program, tag, detail,
/// eval_note)`. `tag`/`detail` are the GATING outcome (JIT vs GHC oracle);
/// `eval_note` is a purely observational eval-vs-oracle note that never
/// affects pass/fail (see the module doc's "Gating policy").
type RunOutcome = (bool, &'static str, String, String);

/// The closure-check and both decodes run inside the worker thread
/// (`Value`/`Env`/`DataConTable` aren't all `Send`-friendly to move back out).
fn run_one(node: &[u8], meta: &[u8], oracle: Option<OracleEntry>) -> RunOutcome {
    let node = node.to_vec();
    let meta = meta.to_vec();
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || -> RunOutcome {
            let expr: CoreExpr = read_cbor(&node).unwrap();
            let table: DataConTable = read_metadata(&meta).unwrap().0;

            // Run both engines INDEPENDENTLY (not via `check_jit_vs_eval_captured` —
            // see the module doc's "Gating policy" for why: its `Agree` variant
            // keeps only eval's value, which can be legitimately WHNF-only).
            let mut heap_eval = VecHeap::new();
            let env_eval = env_from_datacon_table(&table);
            let eval_result: Result<Value, EvalError> = eval(&expr, &env_eval, &mut heap_eval);

            let is_fn = eval_is_closure(&eval_result);
            if is_fn {
                return (true, "MATCH", String::new(), String::new());
            }

            let jit_result: Result<Value, JitError> =
                match JitEffectMachine::compile(&expr, &table, NURSERY) {
                    Ok(mut machine) => machine.run_pure(),
                    Err(e) => Err(e),
                };
            let jit_result: Result<&Value, &JitError> = jit_result.as_ref();

            // Deep-force eval's WHNF value for the observational comparison below:
            // a raw WHNF list (e.g. `cycleTake`'s un-forced tail) can't be decoded
            // into a `list_int`/`string` Canon at all, which would make every
            // lazy-spine binding spuriously "undecodable" instead of an honest
            // eval-vs-GHC value comparison. Irrelevant to the GATING JIT comparison
            // above, which never touches this.
            let eval_result: Result<Value, EvalError> = match eval_result {
                Ok(v) => deep_force(v, &mut heap_eval),
                Err(e) => Err(e),
            };
            let eval_result: Result<&Value, &EvalError> = eval_result.as_ref();

            let (tag, detail): (&'static str, String) = match (&oracle, &jit_result) {
                (None, _) => (
                    "NO-ORACLE",
                    "no oracle.json entry for this binding — GenOracle.hs's manifest needs a new entryXxx line"
                        .to_string(),
                ),
                (Some((kind, oval)), Ok(jv)) => {
                    match (oracle_to_canon(kind, oval), value_to_canon(kind, jv, &table)) {
                        (Ok(oc), Ok(jc)) if oc == jc => ("MATCH", short(jv)),
                        (Ok(oc), Ok(jc)) => {
                            ("DIVERGE", format!("jit={jc:?} ghc={oc:?}"))
                        }
                        (Ok(_), Err(e)) => ("JIT-BUG", format!("JIT value undecodable as {kind}: {e}")),
                        (Err(e), _) => ("NO-ORACLE", format!("oracle.json entry malformed: {e}")),
                    }
                }
                (Some(_), Err(e)) => (
                    if jit_is_gap(e) { "JIT-GAP" } else { "JIT-BUG" },
                    err_short(e),
                ),
            };

            let eval_note = match (&oracle, &eval_result) {
                (None, _) => String::new(),
                (Some((kind, oval)), Ok(ev)) => match (
                    oracle_to_canon(kind, oval),
                    value_to_canon(kind, ev, &table),
                ) {
                    (Ok(oc), Ok(ec)) if oc == ec => "MATCH".to_string(),
                    (Ok(oc), Ok(ec)) => format!("DIVERGE (eval={ec:?} ghc={oc:?})"),
                    (Ok(_), Err(e)) => format!("undecodable ({e})"),
                    (Err(_), _) => String::new(),
                },
                (Some(_), Err(e)) => {
                    if eval_is_gap(e) {
                        format!("GAP ({e:?})")
                    } else {
                        format!("BUG ({e:?})")
                    }
                }
            };

            (false, tag, detail, eval_note)
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
            String::new(),
        ),
    }
}

/// KNOWN non-`MATCH` GATING outcomes (JIT vs the GHC oracle): each is a
/// real-Core bug surfaced by this corpus, documented so a *new* or *changed*
/// divergence fails loudly while these stay green until fixed. `(binding,
/// expected_tag, bug-class)`. Fixing a bug flips it to `MATCH` (which still
/// passes — then prune the stale entry).
///
const KNOWN: &[(&str, &str, &str)] = &[];

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
    let oracle = load_oracle(&dir);

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
    let mut eval_divergences: Vec<String> = Vec::new();

    println!("\n=== REAL-CORE CORPUS (GATE: JIT vs GHC oracle; eval observational) ===");
    for (name, path) in &entries {
        let _guard = tidepool_testing::watchdog::begin(name);
        let node = std::fs::read(path).unwrap();
        let (is_fn, tag, detail, eval_note) = run_one(&node, &meta, oracle.get(name).cloned());
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
        println!("{tag:9} {name:26} {mark:18} {detail}  | eval: {eval_note}");
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
        if eval_note.starts_with("DIVERGE") || eval_note.starts_with("BUG") {
            eval_divergences.push(format!("{name:26} eval: {eval_note}"));
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
        "\n=== DIVERGENCE BUGS vs GHC (implemented-but-wrong / crash / missing oracle) — {} ===",
        bugs.len()
    );
    for b in &bugs {
        println!("  {b}");
    }
    // NON-GATING: eval drifting from GHC (or from the JIT, transitively) is a
    // finding about eval's niche shrinking further, not a JIT regression —
    // logged so it stays visible without failing the corpus.
    println!(
        "\n=== EVAL vs GHC — OBSERVATIONAL ONLY, NEVER GATING — {} ===",
        eval_divergences.len()
    );
    for e in &eval_divergences {
        println!("  {e}");
    }

    assert!(
        violations.is_empty(),
        "UNEXPECTED real-Core outcome(s) — a NEW or CHANGED divergence vs the GHC oracle:\n{}",
        violations.join("\n")
    );
}
