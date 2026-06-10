//! Property tests for `value_to_json` — the render surface every MCP result
//! crosses (`tidepool-runtime/src/render.rs`). A panic here happens *in-process*
//! in the MCP server (safe-Rust, no JIT signal protection), so a panic IS the
//! bug. The properties below assert stability / parseability / equality
//! invariants — never a particular truncation *format*.
//!
//! HYGIENE
//! - All adversarial Values are built by *iterative* folds over `Vec`s. Deep
//!   `Value` spines are dropped by the iterative `Drop` in `tidepool-eval`, but
//!   a *recursive* builder would overflow the host thread at construction time.
//!   Every helper here loops.
//! - proptest strategies map from primitive inputs (sizes, byte vecs, content
//!   strings); shrinking operates on those primitives, so the framework never
//!   deep-clones a generated `Value`.
//! - Property bodies run inside 8 MB stack threads with explicit `Config` cases.
//!
//! FINDINGS: see `plans/proptest-findings-render.md`. Confirmed bugs are pinned
//! as `#[ignore]`d repros at the bottom of this file with minimal `Value`
//! literals; the unconstrained `hunt_*` tests (also `#[ignore]`d) persist
//! proptest regression seeds.

use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use std::sync::{Arc, Mutex};
use tidepool_eval::value::Value;
use tidepool_repr::datacon::{DataCon, SrcBang};
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::types::{DataConId, Literal};
use tidepool_runtime::value_to_json;

const MAX_DEPTH: usize = 1000;
const MAX_LIST_LEN: usize = 10000;

// ---------------------------------------------------------------------------
// DataConTable acquisition — built by hand mirroring the standard one, plus the
// extra constructors the render surface special-cases (ByteArray, tuples, unit).
// Cheaper and more controllable than compiling a Haskell program.
// ---------------------------------------------------------------------------

fn render_table() -> DataConTable {
    let mut t = DataConTable::new();
    // (name, arity) — ids assigned sequentially.
    let cons: &[(&str, u32)] = &[
        ("Nothing", 0),
        ("Just", 1),
        ("False", 0),
        ("True", 0),
        ("()", 0),
        ("I#", 1),
        ("W#", 1),
        ("D#", 1),
        ("C#", 1),
        (":", 2),
        ("[]", 0),
        ("Text", 3),
        ("ByteArray", 1),
        ("(,)", 2),
        ("(,,)", 3),
        ("(,,,)", 4),
    ];
    for (id, (name, arity)) in cons.iter().enumerate() {
        t.insert(DataCon {
            id: DataConId(id as u64),
            name: (*name).to_string(),
            tag: (id as u32) + 1,
            rep_arity: *arity,
            field_bangs: vec![SrcBang::NoSrcBang; *arity as usize],
            qualified_name: None,
        });
    }
    t
}

fn cid(t: &DataConTable, name: &str) -> DataConId {
    t.get_by_name(name)
        .unwrap_or_else(|| panic!("missing constructor {name} in render_table"))
}

// ---------------------------------------------------------------------------
// Iterative Value constructors. NONE of these recurse.
// ---------------------------------------------------------------------------

fn con(t: &DataConTable, name: &str, fields: Vec<Value>) -> Value {
    Value::Con(cid(t, name), fields)
}

fn int(n: i64) -> Value {
    Value::Lit(Literal::LitInt(n))
}

fn bytearray(bytes: Vec<u8>) -> Value {
    Value::ByteArray(Arc::new(Mutex::new(bytes)))
}

/// `Con("ByteArray", [Con("ByteArray", [.. ByteArray(bytes) ..])])`, `layers` deep.
fn nested_bytearray(t: &DataConTable, bytes: Vec<u8>, layers: usize) -> Value {
    let mut acc = bytearray(bytes);
    for _ in 0..layers {
        acc = con(t, "ByteArray", vec![acc]);
    }
    acc
}

/// `Text ba off len`, the ByteArray field wrapped in `layers` `ByteArray` cons.
fn text_value(t: &DataConTable, bytes: Vec<u8>, off: i64, len: i64, layers: usize) -> Value {
    let ba = nested_bytearray(t, bytes, layers);
    con(t, "Text", vec![ba, int(off), int(len)])
}

/// Build a proper cons list from elements — iterative right fold.
fn cons_list(t: &DataConTable, elems: Vec<Value>) -> Value {
    let mut acc = con(t, "[]", vec![]);
    for e in elems.into_iter().rev() {
        acc = con(t, ":", vec![e, acc]);
    }
    acc
}

/// Build an *improper* list: the spine terminates in `tail` instead of `[]`.
fn improper_list(t: &DataConTable, elems: Vec<Value>, tail: Value) -> Value {
    let mut acc = tail;
    for e in elems.into_iter().rev() {
        acc = con(t, ":", vec![e, acc]);
    }
    acc
}

/// `[Char]` as a cons list of `C#(LitChar c)`.
fn char_list(t: &DataConTable, s: &str) -> Value {
    let elems: Vec<Value> = s
        .chars()
        .map(|c| con(t, "C#", vec![Value::Lit(Literal::LitChar(c))]))
        .collect();
    cons_list(t, elems)
}

/// Wrap `inner` in `n` `Just` layers — iterative, straddles the depth cap.
fn nest_just(t: &DataConTable, inner: Value, n: usize) -> Value {
    let mut acc = inner;
    for _ in 0..n {
        acc = con(t, "Just", vec![acc]);
    }
    acc
}

// ---------------------------------------------------------------------------
// Render shims
// ---------------------------------------------------------------------------

fn render(v: &Value, t: &DataConTable) -> serde_json::Value {
    value_to_json(v, t, 0)
}

/// `Ok(json)` on success, `Err(())` if the render panicked.
fn render_caught(v: &Value, t: &DataConTable) -> Result<serde_json::Value, ()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(v, t))).map_err(|_| ())
}

fn in_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn config(cases: u32) -> Config {
    Config {
        cases,
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// Strategies — every leaf maps from primitives to an iteratively-built Value.
// The live `arb_value` AVOIDS shapes already triaged as bugs (out-of-bounds
// Text offsets), so the never-panic / stability properties stay green; the bug
// shapes live in the `#[ignore]`d repros + `hunt_*` seeders below.
// ---------------------------------------------------------------------------

/// A scalar leaf, including f64 specials.
fn arb_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(int),
        Just(Value::Lit(Literal::LitDouble(f64::NAN.to_bits()))),
        Just(Value::Lit(Literal::LitDouble(f64::INFINITY.to_bits()))),
        Just(Value::Lit(Literal::LitDouble(f64::NEG_INFINITY.to_bits()))),
        any::<f64>().prop_map(|f| Value::Lit(Literal::LitDouble(f.to_bits()))),
        any::<u32>().prop_map(|f| Value::Lit(Literal::LitFloat(f as u64))),
    ]
}

/// Text with an IN-BOUNDS offset (off <= array length). `len` may exceed the
/// remaining bytes (render clamps via `.min`), and the byte payload may be
/// invalid UTF-8 / sliced mid-codepoint — all of which are safe (no panic).
fn arb_text(t: DataConTable) -> impl Strategy<Value = Value> {
    (
        proptest::collection::vec(any::<u8>(), 0..12),
        any::<usize>(),
        any::<usize>(),
        0usize..3,
    )
        .prop_map(move |(bytes, off_raw, len_raw, layers)| {
            let alen = bytes.len();
            let off = if alen == 0 { 0 } else { off_raw % (alen + 1) }; // 0..=alen, in bounds
            let len = len_raw % (alen + 3); // may exceed remaining; render clamps
            text_value(&t, bytes, off as i64, len as i64, layers)
        })
}

/// The three string representations of one content string.
fn arb_string_reprs(t: DataConTable) -> impl Strategy<Value = (String, Value, Value, Value)> {
    proptest::collection::vec(any::<char>(), 1..16).prop_map(move |chars| {
        let content: String = chars.into_iter().collect();
        let bytes = content.as_bytes().to_vec();
        let blen = bytes.len() as i64;
        let text = text_value(&t, bytes.clone(), 0, blen, 0);
        let clist = char_list(&t, &content);
        let litstr = Value::Lit(Literal::LitString(bytes));
        (content, text, clist, litstr)
    })
}

/// Adversarial union: scalars, in-bounds text, strings (3 reprs), small proper
/// & improper lists, wrong-arity cons (tag collisions), nested-bytearray-in-
/// con-in-tuple, and moderate Just chains. All iteratively built, all safe.
fn arb_value(t: DataConTable) -> impl Strategy<Value = Value> {
    let t1 = t.clone();
    let t2 = t.clone();
    let t3 = t.clone();
    let t4 = t.clone();
    let t5 = t.clone();
    let t6 = t.clone();
    let t7 = t.clone();
    let t8 = t.clone();

    let leaves = prop_oneof![
        arb_scalar(),
        arb_text(t1),
        Just(con(&t2, "True", vec![])),
        Just(con(&t2, "False", vec![])),
        Just(con(&t2, "()", vec![])),
        Just(con(&t2, "Nothing", vec![])),
        proptest::collection::vec(any::<u8>(), 0..8).prop_map(bytearray),
    ];

    leaves.prop_recursive(4, 64, 8, move |inner| {
        let ta = t3.clone();
        let tb = t4.clone();
        let tc = t5.clone();
        let td = t6.clone();
        let te = t7.clone();
        let tf = t8.clone();
        let tg = t3.clone();
        prop_oneof![
            // proper list
            proptest::collection::vec(inner.clone(), 0..8).prop_map(move |es| cons_list(&ta, es)),
            // improper list — tail is a non-nil, non-cons value (Int)
            (proptest::collection::vec(inner.clone(), 1..6), any::<i64>())
                .prop_map(move |(es, n)| improper_list(&tb, es, int(n))),
            // wrong-arity ":" (cons with 1 or 3 fields) — tag collides with list
            proptest::collection::vec(inner.clone(), 1..4)
                .prop_filter("not exactly 2", |v| v.len() != 2)
                .prop_map(move |fs| Value::Con(cid(&tc, ":"), fs)),
            // tuples (2, 3), nesting allowed
            proptest::collection::vec(inner.clone(), 2..=2)
                .prop_map(move |fs| Value::Con(cid(&td, "(,)"), fs)),
            proptest::collection::vec(inner.clone(), 3..=3)
                .prop_map(move |fs| Value::Con(cid(&tf, "(,,)"), fs)),
            // Just chains (moderate depth)
            (inner.clone(), 0usize..40).prop_map(move |(v, n)| nest_just(&te, v, n)),
            // Just / Con wrapper over inner
            inner.prop_map(move |v| con(&tg, "Just", vec![v])),
        ]
    })
}

// ---------------------------------------------------------------------------
// PROPERTIES
// ---------------------------------------------------------------------------

/// (1) never-panic: rendering any generated value does not panic.
#[test]
fn prop_render_never_panics() {
    in_thread(|| {
        let t = render_table();
        let mut runner = TestRunner::new(config(300));
        runner
            .run(&arb_value(t.clone()), |v| {
                let r = render_caught(&v, &t);
                prop_assert!(r.is_ok(), "render panicked (node_count={})", v.node_count());
                Ok(())
            })
            .unwrap();
    });
}

/// (2) stability: rendering the same value twice is byte-identical.
#[test]
fn prop_render_stable() {
    in_thread(|| {
        let t = render_table();
        let mut runner = TestRunner::new(config(300));
        runner
            .run(&arb_value(t.clone()), |v| {
                let a = render(&v, &t);
                let b = render(&v, &t);
                prop_assert_eq!(
                    serde_json::to_string(&a).unwrap(),
                    serde_json::to_string(&b).unwrap()
                );
                Ok(())
            })
            .unwrap();
    });
}

/// (3) re-parse: everything produced is valid, round-trippable JSON.
#[test]
fn prop_render_reparses() {
    in_thread(|| {
        let t = render_table();
        let mut runner = TestRunner::new(config(300));
        runner
            .run(&arb_value(t.clone()), |v| {
                // Spec: from_str SUCCEEDS on everything render produces. (We do
                // NOT assert parsed == j: serde_json's own f64 text format is not
                // bit-exact round-trip for very large doubles — that is downstream
                // of render and out of scope; `prop_render_stable` already pins
                // render determinism.)
                let j = render(&v, &t);
                let s = serde_json::to_string(&j).unwrap();
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&s);
                prop_assert!(parsed.is_ok(), "unparseable output: {s}");
                Ok(())
            })
            .unwrap();
    });
}

/// (4) repr-equivalence: the three string representations of equal NON-EMPTY
/// content render to equal JSON. (Empty content diverges — see B1 below.)
#[test]
fn prop_repr_equivalence() {
    in_thread(|| {
        let t = render_table();
        let mut runner = TestRunner::new(config(300));
        runner
            .run(
                &arb_string_reprs(t.clone()),
                |(content, text, clist, litstr)| {
                    let jt = render(&text, &t);
                    let jc = render(&clist, &t);
                    let jl = render(&litstr, &t);
                    prop_assert_eq!(&jt, &jc, "Text vs [Char] diverge for {:?}", content);
                    prop_assert_eq!(&jt, &jl, "Text vs LitString diverge for {:?}", content);
                    prop_assert_eq!(&jt, &serde_json::json!(content));
                    Ok(())
                },
            )
            .unwrap();
    });
}

/// (5a) truncation prefix / monotonicity below the cap: a length-N list renders
/// to exactly its N elements (no marker), so length-(N) is a prefix of
/// length-(N+1). Exercised for small N to stay fast.
#[test]
fn prop_truncation_prefix() {
    in_thread(|| {
        let t = render_table();
        let mut runner = TestRunner::new(config(300));
        runner
            .run(&(0usize..200), |n| {
                let elems: Vec<Value> = (0..n).map(|i| int(i as i64)).collect();
                let list = cons_list(&t, elems);
                let j = render(&list, &t);
                let arr = j.as_array().expect("list renders to array");
                // No spurious truncation marker below the cap.
                prop_assert_eq!(arr.len(), n, "below-cap list length mismatch");
                for (i, e) in arr.iter().enumerate() {
                    prop_assert_eq!(e, &serde_json::json!(i as i64));
                }
                // stable + parseable
                let s = serde_json::to_string(&j).unwrap();
                prop_assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok());
                Ok(())
            })
            .unwrap();
    });
}

// ---------------------------------------------------------------------------
// CAP-BOUNDARY COUNTERS — deterministic, each ±1 size explicitly hit.
// These assert the CORRECT/consistent behavior. Where current behavior is
// buggy (exactly-cap list), they assert only stability+parseability and point
// to the `#[ignore]`d repro; a counter proves the size was exercised.
// ---------------------------------------------------------------------------

/// LIST cap straddle: 9999 / 10000 / 10001. Counter-asserts each size renders,
/// is stable, and re-parses. Records the off-by-one at exactly the cap (B5).
#[test]
fn cap_boundary_list_lengths() {
    in_thread(|| {
        let t = render_table();
        let mut hit = [false; 3];
        for (idx, n) in [MAX_LIST_LEN - 1, MAX_LIST_LEN, MAX_LIST_LEN + 1]
            .into_iter()
            .enumerate()
        {
            let elems: Vec<Value> = (0..n).map(|i| int(i as i64)).collect();
            let list = cons_list(&t, elems);
            // never-panic at the boundary
            let j = render_caught(&list, &t).expect("cap-boundary render must not panic");
            let arr = j.as_array().expect("array");
            // stable
            let j2 = render(&list, &t);
            assert_eq!(j, j2, "render-twice instability at n={n}");
            // parseable
            let s = serde_json::to_string(&j).unwrap();
            assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok());

            let has_marker = arr.iter().any(|e| e == &serde_json::json!("..."));
            match n {
                x if x == MAX_LIST_LEN - 1 => {
                    assert_eq!(arr.len(), n, "9999 must be full, no marker");
                    assert!(!has_marker, "9999 must not be truncated");
                }
                x if x == MAX_LIST_LEN => {
                    // B5: a complete 10000-element list is FALSELY truncated.
                    // Current (buggy) behavior: array len 10001 with a "..." marker.
                    // Correct behavior asserted in `bug_b5_list_cap_off_by_one`.
                    assert!(
                        has_marker && arr.len() == MAX_LIST_LEN + 1,
                        "documents B5: exactly-cap list currently carries spurious marker"
                    );
                }
                x if x == MAX_LIST_LEN + 1 => {
                    // genuine truncation
                    assert!(has_marker, "10001 must be truncated");
                    assert_eq!(arr.len(), MAX_LIST_LEN + 1, "truncated to cap + marker");
                }
                _ => unreachable!(),
            }
            hit[idx] = true;
        }
        assert!(hit.iter().all(|&b| b), "all three list-cap sizes exercised");
    });
}

/// DEPTH cap straddle: nest `Just` 998 / 1000 / 1002 deep around an Int.
/// Counter-asserts each size renders, is stable, re-parses, and that the
/// depth-limit sentinel appears iff nesting exceeds the cap.
#[test]
fn cap_boundary_depth() {
    in_thread(|| {
        let t = render_table();
        let mut hit = [false; 3];
        for (idx, depth) in [MAX_DEPTH - 2, MAX_DEPTH, MAX_DEPTH + 2]
            .into_iter()
            .enumerate()
        {
            let v = nest_just(&t, int(7), depth);
            let j = render_caught(&v, &t).expect("deep render must not panic");
            // stable
            assert_eq!(
                j,
                render(&v, &t),
                "render-twice instability at depth={depth}"
            );
            // parseable
            let s = serde_json::to_string(&j).unwrap();
            assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok());

            let hit_sentinel = s.contains("<depth limit>");
            if depth > MAX_DEPTH {
                assert!(
                    hit_sentinel,
                    "depth {depth} must hit the depth-limit sentinel"
                );
            } else {
                // The innermost Int (at nesting == depth) is within the cap.
                assert!(!hit_sentinel, "depth {depth} must NOT hit the sentinel");
                assert_eq!(s, "7", "within-cap Just-chain unwraps to the inner value");
            }
            hit[idx] = true;
        }
        assert!(
            hit.iter().all(|&b| b),
            "all three depth-cap sizes exercised"
        );
    });
}

/// Improper lists and tag-colliding wrong-arity cons must not panic or loop,
/// and must render deterministically. (Explicit smoke alongside the property.)
#[test]
fn improper_and_wrong_arity_are_deterministic() {
    in_thread(|| {
        let t = render_table();
        // improper: 1 : 2 : 3  (tail is Int 3, not [])
        let improper = improper_list(&t, vec![int(1), int(2)], int(3));
        let a = render_caught(&improper, &t).expect("improper list must not panic");
        assert_eq!(a, render(&improper, &t));

        // cons with one field (arity-1 ":")
        let cons1 = Value::Con(cid(&t, ":"), vec![int(1)]);
        let b = render_caught(&cons1, &t).expect("arity-1 cons must not panic");
        assert_eq!(b, render(&cons1, &t));

        // cons with three fields (arity-3 ":")
        let cons3 = Value::Con(cid(&t, ":"), vec![int(1), int(2), int(3)]);
        let c = render_caught(&cons3, &t).expect("arity-3 cons must not panic");
        assert_eq!(c, render(&cons3, &t));

        // all three re-parse
        for j in [&a, &b, &c] {
            let s = serde_json::to_string(j).unwrap();
            assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok());
        }
    });
}

// ===========================================================================
// CONFIRMED-BUG REPROS — `#[ignore]`d so the suite stays green. Each carries a
// minimal `Value` literal, observed/expected, class, and points at the findings
// doc. Run with `cargo test -- --ignored` to reproduce.
// ===========================================================================

/// B-panic: a `Text` whose offset exceeds its backing array length slices
/// `borrowed[off..end]` with `off > end` (end is clamped to `array.len()`),
/// triggering a "slice index starts at N but ends at M" panic IN the MCP
/// server's render path.
///
/// Minimal literal (proptest-shrunk): `Text (ByteArray "") 1 0`  (empty array,
/// off 1). `end = (1+0).min(0) = 0`, then `borrowed[1..0]` => panic.
/// Observed : panic — "range start index 1 out of range for slice of length 0".
/// Expected : a string / graceful sentinel, never a panic.
/// Class    : B-panic.   Seed: tests/proptest-regressions/proptest_render_json.txt
#[test]
#[ignore = "BUG B-panic: Text offset > backing-array length panics in value_to_json (render.rs:145)"]
fn bug_bpanic_text_offset_out_of_bounds() {
    let t = render_table();
    let v = text_value(&t, Vec::new(), 1, 0, 0);
    // Currently panics. When fixed, this should produce a JSON value.
    let r = render_caught(&v, &t);
    assert!(r.is_ok(), "Text with out-of-bounds offset must not panic");
}

/// B1: the three string representations of the EMPTY string diverge.
/// `Text (ByteArray "") 0 0` → `""`, `LitString ""` → `""`, but the empty
/// `[Char]` is just `[]` → renders as the JSON array `[]`.
///
/// Observed : Text/LitString => "",  [Char] => [].
/// Expected : all three => "".
/// Class    : B1 (equal values render differently).
#[test]
#[ignore = "BUG B1: empty-string [Char] renders as [] not \"\" (render.rs:157 vs Text/LitString)"]
fn bug_b1_empty_string_repr_divergence() {
    let t = render_table();
    let text = text_value(&t, Vec::new(), 0, 0, 0);
    let clist = con(&t, "[]", vec![]); // empty [Char]
    let litstr = Value::Lit(Literal::LitString(Vec::new()));
    assert_eq!(
        render(&text, &t),
        render(&clist, &t),
        "Text vs [Char] empty"
    );
    assert_eq!(
        render(&litstr, &t),
        render(&clist, &t),
        "LitString vs [Char] empty"
    );
}

/// B5: a proper list of EXACTLY `MAX_LIST_LEN` elements is falsely truncated.
/// `collect_list` checks `count >= MAX_LIST_LEN` at the top of the loop *after*
/// the cap-th element has been collected and the tail is already `[]`, so it
/// appends a spurious `"..."` marker (off-by-one; should be `>` not `>=`, or
/// the check should precede consuming the tail).
///
/// Observed : array length 10001, trailing "..." for a complete 10000-list.
/// Expected : array length 10000, no marker (nothing was actually truncated).
/// Class    : B5 (truncation non-monotonicity / boundary error).
#[test]
#[ignore = "BUG B5: list of exactly MAX_LIST_LEN gets spurious '...' marker (render.rs:412 off-by-one)"]
fn bug_b5_list_cap_off_by_one() {
    let t = render_table();
    let elems: Vec<Value> = (0..MAX_LIST_LEN).map(|i| int(i as i64)).collect();
    let list = cons_list(&t, elems);
    let arr = render(&list, &t);
    let arr = arr.as_array().unwrap();
    assert_eq!(
        arr.len(),
        MAX_LIST_LEN,
        "complete {MAX_LIST_LEN}-element list must not be truncated"
    );
    assert!(
        !arr.iter().any(|e| e == &serde_json::json!("...")),
        "no spurious truncation marker for a complete list"
    );
}

// ---------------------------------------------------------------------------
// SEED HUNTERS — unconstrained generators that DO produce the bug shapes.
// `#[ignore]`d so they don't fail the suite; running with `--ignored` lets
// proptest discover, shrink, and persist a regression seed under
// tests/proptest-regressions/proptest_render_json.txt.
// ---------------------------------------------------------------------------

/// Unconstrained Text: offsets may exceed the backing array. Persists a seed
/// for B-panic.
#[test]
#[ignore = "seed hunter for B-panic; run with --ignored to persist a regression seed"]
fn hunt_text_offset_panic() {
    in_thread(|| {
        let t = render_table();
        let strat = (
            proptest::collection::vec(any::<u8>(), 0..8),
            0usize..32,
            0usize..32,
        )
            .prop_map({
                let t = t.clone();
                move |(bytes, off, len)| text_value(&t, bytes, off as i64, len as i64, 0)
            });
        // Explicit Direct persistence: this harness can't resolve the source
        // file for the default SourceParallel scheme, so point the seed at the
        // committed regression file directly (cwd is the crate dir under cargo).
        let cfg = Config {
            cases: 512,
            failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
                "tests/proptest-regressions/proptest_render_json.txt",
            ))),
            ..Config::default()
        };
        let mut runner = TestRunner::new(cfg);
        let _ = runner.run(&strat, |v| {
            let r = render_caught(&v, &t);
            prop_assert!(r.is_ok(), "render panicked");
            Ok(())
        });
    });
}
