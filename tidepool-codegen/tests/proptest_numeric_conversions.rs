//! Differential lane: NUMERIC CONVERSIONS x EDGE VALUES.
//!
//! Hardens the JIT (GHC Core -> Cranelift) against the unboxed numeric
//! conversion primops crossed with IEEE-754 / two's-complement edge values that
//! the existing differential suites never deliberately exercise:
//!
//!   int2Double#  double2Int#   int2Float#   float2Int#
//!   double2Float# float2Double# int2Word#    word2Int#
//!   narrow{8,16,32}Int#         narrow{8,16,32}Word#
//!   ord#         chr#
//!
//! crossed with edge seeds (Int / Word / Double / Float pools):
//!   Int:    INT_MIN, INT_MAX, 0, +/-1, +/-2^31, +/-2^53 (the f64 exact-int
//!           boundary), +/-2^24 (the f32 exact-int boundary).
//!   Word:   0, 1, WORD_MAX, 2^63, 2^32, 2^31, 2^16, 2^8.
//!   Double: +/-0.0, +/-Inf, qNaN, sNaN, +/-DBL_MAX, +/-DBL_MIN (smallest
//!           normal), smallest subnormal, 0.5, -0.5, 2^53, -2^53, 2^63,
//!           -2^63, 2^64, fractional values, values just past INT_MAX/MIN.
//!   Float:  the f32 analogues (+/-FLT_MAX, +/-FLT_MIN, subnormal, 2^24, ...).
//!
//! Both implementations are compared bit-exact: `values_equal` compares
//! `Value::Lit` via `Literal`'s derived `PartialEq`, and floats are stored as
//! IEEE bits (`LitDouble(u64)`/`LitFloat(u64)`) — so a NaN-canonicalization or
//! rounding-mode divergence between the tree-walker (Rust `as` casts) and the
//! JIT (Cranelift `fcvt_*` / `ireduce`+`*extend`) is caught, not masked.
//!
//! ORACLE: reuses the project differential oracle `check_jit_vs_eval`
//! (tidepool-testing) so this lane piggybacks on the same JIT-vs-eval contract
//! every other lane uses. We ALSO run a direct in-process comparison so that a
//! divergence's exact JIT-vs-eval `Value`s are surfaced for the report.
//!
//! STRUCTURALLY OUT OF SYNTHETIC REACH (flagged for the build-dependent lane,
//! `tidepool-runtime/tests/proptest_haskell_pipeline.rs`):
//!   - Integer<->Double (`fromIntegral @Integer`, `truncate`/`round`/`floor`/
//!     `ceiling` @Integer, `realToFrac`): there is NO BigNat/Integer constructor
//!     in `standard_datacon_table`, and Integer values are GMP-backed in the
//!     real pipeline. Pure-Int# conversions are fully in reach here; the
//!     boxed-Integer ones are not.
//!   - `rintDouble`/banker's-rounding `round @Double @Int`: lowered to a C FFI
//!     (`rintDouble`) in the real pipeline, not a Core primop — no synthetic
//!     PrimOpKind exists for it, so it can only be reached through Haskell source.

use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use proptest::test_runner::Config;
use serial_test::serial;

use tidepool_repr::types::{Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_eval::value::Value;
use tidepool_eval::{env_from_datacon_table, eval, VecHeap};

use tidepool_codegen::jit_machine::JitEffectMachine;

use tidepool_testing::proptest::{build_table_for_expr, check_jit_vs_eval, values_equal};

// ---------------------------------------------------------------------------
// Reach instrumentation.
// ---------------------------------------------------------------------------
static TOTAL: AtomicU64 = AtomicU64::new(0);
static REACHED: AtomicU64 = AtomicU64::new(0);
static N_SINGLE: AtomicU64 = AtomicU64::new(0);
static N_CHAIN: AtomicU64 = AtomicU64::new(0);

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Types we thread through a conversion chain. Char is a terminal sink only
// (chr# can fail on out-of-range / surrogates; ord# turns Char back into Int).
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumTy {
    Int,
    Word,
    Double,
    Float,
    Char,
}

// ---------------------------------------------------------------------------
// Conversion op catalogue: (op, input ty, output ty). EXACTLY the unboxed
// numeric conversion primops in PrimOpKind. Width-narrowing (`narrow*`) and the
// cross-repr casts are all here; arithmetic ops are intentionally excluded —
// this lane is conversions.
// ---------------------------------------------------------------------------
const CONV_OPS: &[(PrimOpKind, NumTy, NumTy)] = &[
    (PrimOpKind::Int2Double, NumTy::Int, NumTy::Double),
    (PrimOpKind::Double2Int, NumTy::Double, NumTy::Int),
    (PrimOpKind::Int2Float, NumTy::Int, NumTy::Float),
    (PrimOpKind::Float2Int, NumTy::Float, NumTy::Int),
    (PrimOpKind::Double2Float, NumTy::Double, NumTy::Float),
    (PrimOpKind::Float2Double, NumTy::Float, NumTy::Double),
    (PrimOpKind::Int2Word, NumTy::Int, NumTy::Word),
    (PrimOpKind::Word2Int, NumTy::Word, NumTy::Int),
    (PrimOpKind::Narrow8Int, NumTy::Int, NumTy::Int),
    (PrimOpKind::Narrow16Int, NumTy::Int, NumTy::Int),
    (PrimOpKind::Narrow32Int, NumTy::Int, NumTy::Int),
    (PrimOpKind::Narrow8Word, NumTy::Word, NumTy::Word),
    (PrimOpKind::Narrow16Word, NumTy::Word, NumTy::Word),
    (PrimOpKind::Narrow32Word, NumTy::Word, NumTy::Word),
    (PrimOpKind::Ord, NumTy::Char, NumTy::Int),
    // chr# is the only op that can dynamically fail (invalid codepoint) — both
    // impls reject the same set, so eval-Err / JIT-trap agree (both fail).
    (PrimOpKind::Chr, NumTy::Int, NumTy::Char),
];

// ---------------------------------------------------------------------------
// Edge-value seed pools, one per source type. These are the load-bearing
// inputs: the corners where Rust `as` casts and Cranelift `fcvt_*` are most
// likely to disagree.
// ---------------------------------------------------------------------------

fn int_seeds() -> Vec<i64> {
    vec![
        0,
        1,
        -1,
        2,
        -2,
        i64::MAX, // 9223372036854775807
        i64::MIN, // -9223372036854775808
        i64::MAX - 1,
        i64::MIN + 1,
        1 << 31, // 2^31
        -(1i64 << 31),
        (1 << 31) - 1, // INT32_MAX
        -(1i64 << 31) - 1,
        1 << 24,       // 2^24 — f32 exact-int boundary
        (1 << 24) + 1, // first f32-inexact int
        -(1i64 << 24) - 1,
        1 << 53,       // 2^53 — f64 exact-int boundary
        (1 << 53) + 1, // first f64-inexact int
        -(1i64 << 53) - 1,
        1 << 62,
        -(1i64 << 62),
        255,
        256,
        -128,
        -129,
        65535,
        65536,
        -32768,
        -32769,
        0x10FFFF, // max Unicode codepoint
        0x110000, // first invalid codepoint (chr# domain edge)
        0xD7FF,
        0xD800, // first surrogate (chr# rejects)
        0xDFFF, // last surrogate
        0xE000,
        65, // 'A'
    ]
}

fn word_seeds() -> Vec<u64> {
    vec![
        0,
        1,
        2,
        u64::MAX, // WORD_MAX
        u64::MAX - 1,
        1 << 63, // 2^63
        (1 << 63) - 1,
        (1 << 63) + 1,
        1 << 32,       // 2^32
        (1 << 32) - 1, // WORD32_MAX
        1 << 31,
        1 << 16,
        (1 << 16) - 1,
        1 << 8,
        (1 << 8) - 1,
        255,
        256,
        0x8000_0000_0000_0000,
        0xFFFF_FFFF_FFFF_FFFF,
    ]
}

fn double_seeds() -> Vec<u64> {
    // Stored as raw bits to capture distinct NaN encodings precisely.
    let mut v: Vec<u64> = vec![
        0.0f64.to_bits(),
        (-0.0f64).to_bits(),
        1.0f64.to_bits(),
        (-1.0f64).to_bits(),
        0.5f64.to_bits(),
        (-0.5f64).to_bits(),
        2.5f64.to_bits(),
        (-2.5f64).to_bits(),
        1.5f64.to_bits(),
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        f64::MAX.to_bits(),          // ~1.797e308
        f64::MIN.to_bits(),          // -1.797e308
        f64::MIN_POSITIVE.to_bits(), // smallest normal ~2.225e-308
        f64::EPSILON.to_bits(),
        1.0e308f64.to_bits(),
        (-1.0e308f64).to_bits(),
        1.0e-308f64.to_bits(),
        (1u64 << 52).reverse_bits(), // arbitrary mid-range bit soup
    ];
    // Specific NaN encodings: quiet NaN, signalling NaN, NaN with payload,
    // negative-sign NaN. fcvt and `as` casts must agree on what Int these map
    // to (0) AND, where NaN is preserved (float<->double widen/narrow), on the
    // bit pattern.
    v.push(0x7FF8_0000_0000_0000); // canonical qNaN
    v.push(0x7FF0_0000_0000_0001); // sNaN (payload, quiet bit clear)
    v.push(0xFFF8_0000_0000_0000); // negative qNaN
    v.push(0x7FFF_FFFF_FFFF_FFFF); // NaN, all-payload
    v.push(0xFFF0_0000_0000_0001); // negative sNaN
                                   // Exactly-representable large integers and just-past-range values.
    v.push((9223372036854775807.0f64).to_bits()); // ~ INT_MAX as f64 (rounds to 2^63)
    v.push((-9223372036854775808.0f64).to_bits()); // INT_MIN exactly (= -2^63)
    v.push((9223372036854775808.0f64).to_bits()); // 2^63 exactly — overflows i64
    v.push((-9223372036854775809.0f64).to_bits()); // just below INT_MIN
    v.push((1.8446744073709552e19f64).to_bits()); // ~2^64 — overflows i64 and u64
    v.push((4503599627370496.0f64).to_bits()); // 2^52
    v.push((9007199254740992.0f64).to_bits()); // 2^53
    v.push((16777216.0f64).to_bits()); // 2^24 (f32 boundary, exact in f64)
    v.push((16777217.0f64).to_bits()); // 2^24 + 1 (inexact when demoted to f32)
    v.push((3.4028235e38f64).to_bits()); // ~ FLT_MAX (overflows f32 -> Inf? boundary)
    v.push((3.5e38f64).to_bits()); // past FLT_MAX -> +Inf on demote
    v.push((-3.5e38f64).to_bits()); // past -FLT_MAX -> -Inf on demote
    v.push((1.0e-40f64).to_bits()); // subnormal range for f32 (demote loses precision)
    v.push((1.0e-46f64).to_bits()); // underflows f32 to 0 / smallest subnormal
    v
}

fn float_seeds() -> Vec<u64> {
    // Stored as the low 32 bits in a u64, matching Literal::LitFloat.
    let mut v: Vec<u64> = vec![
        0.0f32.to_bits() as u64,
        (-0.0f32).to_bits() as u64,
        1.0f32.to_bits() as u64,
        (-1.0f32).to_bits() as u64,
        0.5f32.to_bits() as u64,
        (-0.5f32).to_bits() as u64,
        2.5f32.to_bits() as u64,
        f32::INFINITY.to_bits() as u64,
        f32::NEG_INFINITY.to_bits() as u64,
        f32::MAX.to_bits() as u64,          // ~3.4e38
        f32::MIN.to_bits() as u64,          // -3.4e38
        f32::MIN_POSITIVE.to_bits() as u64, // smallest normal ~1.175e-38
        f32::EPSILON.to_bits() as u64,
    ];
    v.push(0x7FC0_0000); // canonical qNaN (f32)
    v.push(0x7F80_0001); // sNaN (f32)
    v.push(0xFFC0_0000); // negative qNaN (f32)
    v.push(0x7FFF_FFFF); // NaN, all-payload (f32)
    v.push(0x0000_0001); // smallest positive subnormal (f32)
    v.push(0x8000_0001); // smallest negative subnormal (f32)
    v.push(0x007F_FFFF); // largest subnormal (f32)
    v.push((16777216.0f32).to_bits() as u64); // 2^24 — f32 exact-int boundary
    v.push((9.223372e18f32).to_bits() as u64); // ~ INT_MAX region (overflows i64 conv? boundary)
    v.push((1.8446744e19f32).to_bits() as u64); // ~2^64
    v.push((3.4e38f32).to_bits() as u64); // near FLT_MAX, overflows i64
    v.push((-3.4e38f32).to_bits() as u64);
    v.push((12345.678f32).to_bits() as u64);
    v.push((-12345.678f32).to_bits() as u64);
    v
}

// ---------------------------------------------------------------------------
// Build a seed leaf of a given type.
// ---------------------------------------------------------------------------
fn push_seed(b: &mut TreeBuilder, ty: NumTy, raw: u64) -> usize {
    let lit = match ty {
        NumTy::Int => Literal::LitInt(raw as i64),
        NumTy::Word => Literal::LitWord(raw),
        NumTy::Double => Literal::LitDouble(raw),
        NumTy::Float => Literal::LitFloat(raw & 0xFFFF_FFFF),
        NumTy::Char => {
            // Char seeds come in as a codepoint in `raw`; only valid ones reach
            // here (the generator restricts Char seeds to valid codepoints).
            let cp = (raw as u32) & 0x1F_FFFF;
            let c = char::from_u32(cp).unwrap_or('A');
            Literal::LitChar(c)
        }
    };
    b.push(CoreFrame::Lit(lit))
}

// A spec is: a typed seed plus an ordered chain of conversion ops, each of which
// type-checks against the running type. The result is whatever the last op
// produces — always a single Lit, so always bit-exact comparable.
#[derive(Clone, Debug)]
struct ConvSpec {
    seed_ty: NumTy,
    seed_raw: u64,
    ops: Vec<PrimOpKind>,
}

fn build_conv(spec: &ConvSpec) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let mut cur = push_seed(&mut b, spec.seed_ty, spec.seed_raw);
    let mut cur_ty = spec.seed_ty;
    for &op in &spec.ops {
        let (_, in_ty, out_ty) = CONV_OPS
            .iter()
            .find(|(o, _, _)| *o == op)
            .copied()
            .expect("op in catalogue");
        debug_assert_eq!(in_ty, cur_ty, "chain type mismatch building conv");
        cur = b.push(CoreFrame::PrimOp {
            op,
            args: vec![cur],
        });
        cur_ty = out_ty;
    }
    let tree = b.build();
    // Root must be the last node; the single-spine construction guarantees it.
    debug_assert_eq!(tree.nodes.len() - 1, cur);
    tree
}

// ---------------------------------------------------------------------------
// Generators.
// ---------------------------------------------------------------------------

/// A typed edge seed: pick a type, then an edge value from that type's pool.
fn arb_seed() -> impl Strategy<Value = (NumTy, u64)> {
    prop_oneof![
        prop::sample::select(int_seeds()).prop_map(|i| (NumTy::Int, i as u64)),
        prop::sample::select(word_seeds()).prop_map(|w| (NumTy::Word, w)),
        prop::sample::select(double_seeds()).prop_map(|d| (NumTy::Double, d)),
        prop::sample::select(float_seeds()).prop_map(|f| (NumTy::Float, f)),
        // Char seed: a valid codepoint, fed as Char (ord# is the only consumer).
        prop::sample::select(vec![
            0u64, 65, 0x7F, 0x80, 0xFF, 0x100, 0xD7FF, 0xE000, 0xFFFF, 0x10000, 0x10FFFF,
        ])
        .prop_map(|c| (NumTy::Char, c)),
    ]
}

/// Grow a type-correct conversion chain of length `len` from a starting type.
fn arb_chain_for(start_ty: NumTy, len: usize) -> impl Strategy<Value = Vec<PrimOpKind>> {
    // Build the chain greedily/randomly: at each step choose among ops whose
    // input type matches the running type. Encoded as a vector of selection
    // indices so proptest can shrink.
    prop::collection::vec(0usize..CONV_OPS.len(), len).prop_map(move |choices| {
        let mut out = Vec::new();
        let mut ty = start_ty;
        for &c in &choices {
            // candidate ops for the current type
            let candidates: Vec<(PrimOpKind, NumTy)> = CONV_OPS
                .iter()
                .filter(|(_, i, _)| *i == ty)
                .map(|(o, _, out)| (*o, *out))
                .collect();
            if candidates.is_empty() {
                break; // dead end (e.g. ran into Char with no ord#); stop here
            }
            let (op, out_ty) = candidates[c % candidates.len()];
            out.push(op);
            ty = out_ty;
        }
        out
    })
}

fn arb_single() -> impl Strategy<Value = ConvSpec> {
    arb_seed().prop_flat_map(|(seed_ty, seed_raw)| {
        arb_chain_for(seed_ty, 1).prop_map(move |ops| ConvSpec {
            seed_ty,
            seed_raw,
            ops,
        })
    })
}

fn arb_chain() -> impl Strategy<Value = ConvSpec> {
    (arb_seed(), 2usize..6).prop_flat_map(|((seed_ty, seed_raw), len)| {
        arb_chain_for(seed_ty, len).prop_map(move |ops| ConvSpec {
            seed_ty,
            seed_raw,
            ops,
        })
    })
}

// ---------------------------------------------------------------------------
// Oracle wrapper: run the shared differential oracle at two nursery sizes, and
// also do a direct comparison so a divergence prints both Values cleanly.
// ---------------------------------------------------------------------------
fn run_oracle(expr: CoreExpr) -> Result<(), TestCaseError> {
    bump(&TOTAL);

    // Direct compare (also surfaces the exact Values on mismatch).
    let table = build_table_for_expr(&expr);
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let ev = eval(&expr, &env, &mut heap);
    let jit = JitEffectMachine::compile(&expr, &table, 64 * 1024).and_then(|mut m| m.run_pure());

    if let (Ok(v1), Ok(v2)) = (&ev, &jit) {
        prop_assert!(
            values_equal(v1, v2),
            "NUMERIC-CONV DIVERGENCE.\nEval: {:?}\nJIT:  {:?}\nExpr: {:#?}",
            v1,
            v2,
            expr
        );
        // Surface a JIT-only success/failure mismatch loudly (B2 class).
    } else if ev.is_ok() != jit.is_ok() {
        // Conversions never legitimately diverge on success-vs-error: every op
        // is total except chr#, and chr# rejects the SAME domain in both impls.
        // So a one-sided result is a real divergence.
        prop_assert!(
            false,
            "NUMERIC-CONV ONE-SIDED RESULT (B2).\nEval: {:?}\nJIT:  {:?}\nExpr: {:#?}",
            ev,
            jit,
            expr
        );
    }
    if ev.is_ok() && jit.is_ok() {
        bump(&REACHED);
    }

    // Shared oracle, both nursery sizes (B1/B2/B4 contract; chr#-domain errors
    // both-fail so they're skipped, not a divergence).
    check_jit_vs_eval(expr.clone(), 64 * 1024)?;
    check_jit_vs_eval(expr, 4 * 1024)?;
    Ok(())
}

fn cfg(cases: u32) -> Config {
    let mut c = Config::with_cases(cases);
    c.max_shrink_iters = 8000;
    c
}

proptest! {
    #![proptest_config(cfg(400))]

    /// Single conversion op over an edge value. Highest signal: every divergence
    /// here is one op + one value.
    #[test]
    #[serial]
    fn prop_single_conversion(spec in arb_single()) {
        bump(&N_SINGLE);
        let expr = build_conv(&spec);
        run_oracle(expr)?;
    }
}

proptest! {
    #![proptest_config(cfg(500))]

    /// Conversion CHAINS (depth 2..6): int2Double -> double2Float -> float2Int
    /// etc. Stresses round-trip error accumulation, repeated NaN propagation,
    /// and width-narrowing interleaved with repr casts.
    #[test]
    #[serial]
    fn prop_conversion_chains(spec in arb_chain()) {
        bump(&N_CHAIN);
        let expr = build_conv(&spec);
        run_oracle(expr)?;
    }
}

// ===========================================================================
// EXHAUSTIVE deterministic sweep: every single conversion op applied to every
// edge value of its input type. This is fully enumerable (no randomness) and is
// the bedrock of the lane — proptest adds the chains and the cross-products.
//
// A failure here aborts with the precise (op, value, jit, eval) tuple. We do NOT
// stop at the first divergence: we collect them all so one run names the whole
// failing set.
// ===========================================================================

fn run_single(op: PrimOpKind, in_ty: NumTy, raw: u64) -> Result<(Value, Value), (String, String)> {
    let mut b = TreeBuilder::new();
    let seed = push_seed(&mut b, in_ty, raw);
    let _root = b.push(CoreFrame::PrimOp {
        op,
        args: vec![seed],
    });
    let expr = b.build();
    let table = build_table_for_expr(&expr);
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let ev = eval(&expr, &env, &mut heap);
    let jit = JitEffectMachine::compile(&expr, &table, 64 * 1024).and_then(|mut m| m.run_pure());
    match (ev, jit) {
        (Ok(v1), Ok(v2)) => Ok((v1, v2)),
        (Err(_), Err(_)) => Err(("both-error".into(), "both-error".into())),
        (e, j) => Err((format!("{:?}", e), format!("{:?}", j))),
    }
}

fn seeds_for(ty: NumTy) -> Vec<u64> {
    match ty {
        NumTy::Int => int_seeds().into_iter().map(|i| i as u64).collect(),
        NumTy::Word => word_seeds(),
        NumTy::Double => double_seeds(),
        NumTy::Float => float_seeds(),
        NumTy::Char => vec![
            0, 65, 0x7F, 0x80, 0xFF, 0x100, 0xD7FF, 0xE000, 0xFFFF, 0x10000, 0x10FFFF,
        ],
    }
}

#[test]
#[serial]
fn exhaustive_single_op_x_edge_values() {
    let mut divergences: Vec<String> = Vec::new();
    let mut compared = 0u64;
    let mut both_err = 0u64;

    for &(op, in_ty, _out_ty) in CONV_OPS {
        for raw in seeds_for(in_ty) {
            match run_single(op, in_ty, raw) {
                Ok((v1, v2)) => {
                    compared += 1;
                    if !values_equal(&v1, &v2) {
                        divergences.push(format!(
                            "{:?}(seed_ty={:?}, raw=0x{:016X}): eval={:?} jit={:?}",
                            op, in_ty, raw, v1, v2
                        ));
                    }
                }
                Err((e, j)) => {
                    if e == "both-error" {
                        both_err += 1;
                    } else {
                        divergences.push(format!(
                            "{:?}(seed_ty={:?}, raw=0x{:016X}) ONE-SIDED: eval={} jit={}",
                            op, in_ty, raw, e, j
                        ));
                    }
                }
            }
        }
    }

    eprintln!(
        "EXHAUSTIVE numeric-conv sweep: compared={compared}, both_error={both_err}, \
         divergences={}",
        divergences.len()
    );
    assert!(
        divergences.is_empty(),
        "Numeric-conversion divergences ({}):\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// Reach floor: at least 90% of attempted proptest cases reached value
/// comparison (conversions are total bar chr#, so this should be very high).
/// `zzz_` orders it last within the file (proptest runs tests alphabetically).
#[test]
#[serial]
fn zzz_reach_floor() {
    let total = TOTAL.load(Ordering::Relaxed);
    let reached = REACHED.load(Ordering::Relaxed);
    eprintln!(
        "NUMERIC-CONV REACH: {}/{} cases reached value comparison ({:.1}%) [single={} chain={}]",
        reached,
        total,
        if total > 0 {
            100.0 * reached as f64 / total as f64
        } else {
            0.0
        },
        N_SINGLE.load(Ordering::Relaxed),
        N_CHAIN.load(Ordering::Relaxed),
    );
    if total >= 100 {
        let ratio = reached as f64 / total as f64;
        assert!(
            ratio >= 0.90,
            "reach floor: only {:.1}% of {} cases reached value comparison (need >= 90%)",
            100.0 * ratio,
            total
        );
    }
}
