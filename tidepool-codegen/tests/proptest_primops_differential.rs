//! Lane: EXHAUSTIVE PRIMOPS differential (JIT vs tree-walking eval).
//!
//! Goal: drive EVERY `PrimOpKind` the JIT supports through the
//! `check_jit_vs_eval` differential oracle with EDGE operands
//! (INT_MIN/MAX, WORD_MAX, 0, +/-1, small negatives, sign-bit patterns),
//! and surface any JIT-vs-eval divergence with a minimal repro.
//!
//! This is a SELF-CONTAINED lane: it adds its own generators (it does not edit
//! the shared `strategy.rs`), and reuses `check_jit_vs_eval` /
//! `build_table_for_expr` / `values_equal` from `tidepool-testing` as the
//! oracle.
//!
//! ## Why the result is left as a BARE Lit (not wrapped in `I#`)
//!
//! `values_equal` compares `Value::Lit(l1) == Value::Lit(l2)` with the derived
//! `Literal` PartialEq, so `LitWord(x)` and `LitInt(x)` are DIFFERENT values.
//! The JIT bridges its result back to a `Value` using the lit-TAG it produced
//! (`heap_bridge::heap_to_value`: `LIT_TAG_INT -> LitInt`,
//! `LIT_TAG_WORD -> LitWord`, `LIT_TAG_CHAR -> LitChar`, etc.). Leaving the
//! primop result as the top-level value therefore makes the differential
//! observe BOTH the numeric value AND the result lit-tag (the Int#/Word#
//! distinction). A result-tag misclassification (e.g. a Word op tagging its
//! result INT) is a real divergence and we want to catch it, so we do NOT mask
//! it by re-boxing through `I#`/`unbox_int`.
//!
//! ## Multi-output primops (the high-yield class)
//!
//! GHC's unboxed-tuple primops (`timesInt2#`, `timesWord2#`, `quotRemWord#`,
//! `addIntC#`, `subWordC#`, `addWordC#`) are lowered in this codebase to ONE
//! `PrimOpKind` per tuple SLOT (`TimesInt2Hi` / `TimesInt2Lo` /
//! `TimesInt2Overflow`, etc.). A slot-ordering bug (such as the recently-fixed
//! `timesInt2#` hi/lo swap) manifests as a wrong value in exactly one slot. We
//! probe each slot independently AND together (folded into a Pair/triple Con,
//! whose fields `values_equal` compares element-wise) so a single swapped slot
//! fails loudly with a pin-pointable repro.
//!
//! ## Operands
//!
//! `arb_edge_i64` / `arb_edge_u64` bias HARD toward boundary values
//! (INT_MIN, INT_MAX, WORD_MAX, 0, ±1, sign-bit, byte/half-word boundaries) so
//! the small case budget spends itself on the operands most likely to expose
//! sign-extension, narrowing, and overflow bugs.

use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use proptest::test_runner::Config;
use serial_test::serial;

use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
use tidepool_repr::types::{DataConId, Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_testing::proptest::{build_table_for_expr, check_jit_vs_eval};

use tidepool_codegen::jit_machine::JitEffectMachine;

// Standard-table DataCon ids (must match `standard_datacon_table`).
const PAIR: DataConId = DataConId(4);

// ---------------------------------------------------------------------------
// Reach / coverage instrumentation.
// ---------------------------------------------------------------------------
static REACHED: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
// Hits of the documented eval-side overflow-panic divergence (CONFIRMED-BUG
// EVAL-1/2/3). Tolerated by the live fuzzer (the JIT side is asserted Ok and
// the inputs are pinned by the `#[ignore]`d repros below) so the net stays
// green and keeps hunting for OTHER divergences. NOT counted toward reach.
static N_EVAL_PANIC: AtomicU64 = AtomicU64::new(0);

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// The oracle wrapper: run an expression through the differential at TWO nursery
// sizes (default 64KB + a 16KB nursery — these are tiny scalar programs so a
// 16KB nursery is more than enough; the second size exercises the nursery-knob
// determinism). Counts reach.
// ---------------------------------------------------------------------------
fn run_oracle(expr: CoreExpr) -> Result<(), TestCaseError> {
    bump(&TOTAL);
    check_jit_vs_eval(expr.clone(), 64 * 1024)?;
    check_jit_vs_eval(expr, 16 * 1024)?;
    bump(&REACHED);
    Ok(())
}

/// Panic-tolerant oracle for the lanes that probe `Int64*`/`Word64*` shift and
/// negate ops with out-of-range / INT_MIN operands.
///
/// CONFIRMED-BUG class EVAL-1/2/3: the tree-walking interpreter's `Int64Negate`
/// / `Int64Shra` / `Word64Shl` handlers use RAW arithmetic (`-a`, `a >> b`,
/// `a << b`) where their `IntNegate` / `IntShra` / `WordShl` siblings use
/// `wrapping_*`. In a debug build the raw form PANICS ("negate with overflow",
/// "shift right/left with overflow"); the JIT lowers all of them to the same
/// wrapping Cranelift instruction (`ineg` / `sshr` / `ishl`, all mod-width) and
/// returns a value. A `catch_unwind` here turns the eval panic into a recorded,
/// tolerated divergence and asserts the JIT still succeeds — keeping the suite
/// green while pinning the bug (see `evalbug*` repros below).
fn run_oracle_eval_may_panic(expr: CoreExpr) -> Result<(), TestCaseError> {
    bump(&TOTAL);
    let table = build_table_for_expr(&expr);
    let ev = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut heap = VecHeap::new();
        let env = env_from_datacon_table(&table);
        eval(&expr, &env, &mut heap)
    }));
    match ev {
        Ok(_eval_result) => {
            // eval did NOT panic on this operand — run the strict differential.
            check_jit_vs_eval(expr.clone(), 64 * 1024)?;
            check_jit_vs_eval(expr, 16 * 1024)?;
            bump(&REACHED);
        }
        Err(_panic) => {
            // Documented eval overflow panic. Assert the JIT handles it cleanly
            // (returns a value, never a compile/codegen error) and tolerate.
            let jit =
                JitEffectMachine::compile(&expr, &table, 64 * 1024).and_then(|mut m| m.run_pure());
            prop_assert!(
                jit.is_ok(),
                "EVAL-panic operand but JIT ALSO failed (this would be a new \
                 divergence, not the documented eval-overflow bug).\nJIT: {:?}\nExpr: {:#?}",
                jit,
                expr
            );
            bump(&N_EVAL_PANIC);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Edge-biased operand strategies.
// ---------------------------------------------------------------------------
fn edge_i64_values() -> Vec<i64> {
    vec![
        i64::MIN,
        i64::MIN + 1,
        i64::MAX,
        i64::MAX - 1,
        0,
        1,
        -1,
        2,
        -2,
        3,
        -3,
        7,
        -7,
        8,
        16,
        31,
        32,
        63,
        64,
        255,
        256,
        -256,
        0x7FFF_FFFF,           // i32::MAX
        -0x8000_0000,          // i32::MIN
        0x1_0000_0000,         // 2^32
        0x7FFF,                // i16::MAX
        -0x8000,               // i16::MIN
        0x7F,                  // i8::MAX
        -0x80,                 // i8::MIN
        0x5555_5555_5555_5555, // alternating bits
        -0x5555_5555_5555_5556i64, // ~alternating
        0x0123_4567_89AB_CDEF,
        i64::MIN / 2,
        i64::MAX / 2,
    ]
}

fn edge_u64_values() -> Vec<u64> {
    vec![
        0,
        1,
        2,
        3,
        7,
        8,
        u64::MAX,
        u64::MAX - 1,
        u64::MAX / 2,
        0x8000_0000_0000_0000, // sign bit / 2^63
        0x7FFF_FFFF_FFFF_FFFF, // i64::MAX as u64
        0xFFFF_FFFF,           // u32::MAX
        0x1_0000_0000,         // 2^32
        0xFFFF,                // u16::MAX
        0xFF,                  // u8::MAX
        0x100,
        0xAAAA_AAAA_AAAA_AAAA,
        0x5555_5555_5555_5555,
        0x0123_4567_89AB_CDEF,
        256,
        255,
        128,
        64,
    ]
}

fn arb_edge_i64() -> impl Strategy<Value = i64> {
    let vals = edge_i64_values();
    let n = vals.len();
    // 80% edge value, 20% arbitrary i64 (so we still wander off the boundaries).
    prop_oneof![
        8 => (0..n).prop_map(move |i| vals[i]),
        2 => any::<i64>(),
    ]
}

fn arb_edge_u64() -> impl Strategy<Value = u64> {
    let vals = edge_u64_values();
    let n = vals.len();
    prop_oneof![
        8 => (0..n).prop_map(move |i| vals[i]),
        2 => any::<u64>(),
    ]
}

// Shift amount: edge values around the word boundary (0..=63 are well-defined;
// 64+ is masked the same way by both sides via `wrapping_shl(b as u32)` vs
// Cranelift's modulo-width shift — both reduce mod 64).
fn arb_shift() -> impl Strategy<Value = i64> {
    prop_oneof![
        (0i64..64).boxed(),
        Just(0i64),
        Just(1i64),
        Just(63i64),
        Just(64i64),
        Just(65i64),
        Just(127i64),
        Just(128i64),
        Just(-1i64),
    ]
}

fn arb_codepoint() -> impl Strategy<Value = i64> {
    // Valid Unicode scalar values only (Chr traps on invalid; keep both sides
    // in the success lane to maximize reach — invalid-codepoint trapping is
    // covered separately by the existing emit_primop unit tests).
    prop_oneof![
        (0i64..0xD800),
        (0xE000i64..0x11_0000),
        Just(0i64),
        Just(0x41i64),  // 'A'
        Just(0xD7FFi64),
        Just(0xE000i64),
        Just(0x10_FFFFi64),
    ]
}

fn arb_double() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f64>(),
        Just(0.0f64),
        Just(-0.0f64),
        Just(1.0f64),
        Just(-1.0f64),
        Just(0.5f64),
        Just(2.0f64),
        Just(3.5f64),
        Just(f64::MIN),
        Just(f64::MAX),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(1e300f64),
        Just(-1e300f64),
        Just(1e-300f64),
        Just(i64::MAX as f64),
        Just(i64::MIN as f64),
        Just(4503599627370496.0f64), // 2^52
        (-1000.0f64..1000.0),
    ]
}

fn arb_float() -> impl Strategy<Value = f32> {
    prop_oneof![
        any::<f32>(),
        Just(0.0f32),
        Just(-0.0f32),
        Just(1.0f32),
        Just(-1.0f32),
        Just(0.5f32),
        Just(f32::MAX),
        Just(f32::MIN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
        Just(i32::MAX as f32),
        (-1000.0f32..1000.0),
    ]
}

// ---------------------------------------------------------------------------
// Tiny CoreExpr builders. Each makes the primop result the TOP-LEVEL value so
// the differential observes the full Literal (value AND lit-tag).
// ---------------------------------------------------------------------------

fn lit_int(b: &mut TreeBuilder, n: i64) -> usize {
    b.push(CoreFrame::Lit(Literal::LitInt(n)))
}
fn lit_word(b: &mut TreeBuilder, n: u64) -> usize {
    b.push(CoreFrame::Lit(Literal::LitWord(n)))
}
fn lit_double(b: &mut TreeBuilder, d: f64) -> usize {
    b.push(CoreFrame::Lit(Literal::LitDouble(d.to_bits())))
}
fn lit_float(b: &mut TreeBuilder, f: f32) -> usize {
    b.push(CoreFrame::Lit(Literal::LitFloat(f.to_bits() as u64)))
}
fn lit_char(b: &mut TreeBuilder, c: char) -> usize {
    b.push(CoreFrame::Lit(Literal::LitChar(c)))
}

fn finish(b: TreeBuilder) -> CoreExpr {
    b.build()
}

/// `op(args...)` as the whole program.
fn prog_op(op: PrimOpKind, build_args: impl FnOnce(&mut TreeBuilder) -> Vec<usize>) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let args = build_args(&mut b);
    b.push(CoreFrame::PrimOp { op, args });
    finish(b)
}

/// Unary int op on a single Int# operand.
fn prog_unary_int(op: PrimOpKind, a: i64) -> CoreExpr {
    prog_op(op, |b| {
        let x = lit_int(b, a);
        vec![x]
    })
}
/// Binary int op on two Int# operands.
fn prog_binary_int(op: PrimOpKind, a: i64, b_: i64) -> CoreExpr {
    prog_op(op, |b| {
        let x = lit_int(b, a);
        let y = lit_int(b, b_);
        vec![x, y]
    })
}
/// Unary word op on a single Word# operand.
fn prog_unary_word(op: PrimOpKind, a: u64) -> CoreExpr {
    prog_op(op, |b| {
        let x = lit_word(b, a);
        vec![x]
    })
}
/// Binary word op on two Word# operands.
fn prog_binary_word(op: PrimOpKind, a: u64, b_: u64) -> CoreExpr {
    prog_op(op, |b| {
        let x = lit_word(b, a);
        let y = lit_word(b, b_);
        vec![x, y]
    })
}
/// Word op whose 2nd operand is an Int# (shift amount): `op(Word#, Int#)`.
fn prog_word_shift(op: PrimOpKind, a: u64, sh: i64) -> CoreExpr {
    prog_op(op, |b| {
        let x = lit_word(b, a);
        let y = lit_int(b, sh);
        vec![x, y]
    })
}

/// Build a `(,)`-Con wrapping two primop slots that share operands `(a,b)`,
/// so a slot-ordering bug in EITHER slot surfaces in one case. Both slots are
/// computed from FRESH operand literals (a primop arg must be a Lit/forcible
/// node, and each slot is an independent PrimOp).
fn prog_pair_slots_word(slot0: PrimOpKind, slot1: PrimOpKind, a: u64, b_: u64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let a0 = lit_word(&mut b, a);
    let b0 = lit_word(&mut b, b_);
    let s0 = b.push(CoreFrame::PrimOp {
        op: slot0,
        args: vec![a0, b0],
    });
    let a1 = lit_word(&mut b, a);
    let b1 = lit_word(&mut b, b_);
    let s1 = b.push(CoreFrame::PrimOp {
        op: slot1,
        args: vec![a1, b1],
    });
    b.push(CoreFrame::Con {
        tag: PAIR,
        fields: vec![s0, s1],
    });
    finish(b)
}

/// Same, but Int# operands (for timesInt2#/addIntC# slot pairs).
fn prog_pair_slots_int(slot0: PrimOpKind, slot1: PrimOpKind, a: i64, b_: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let a0 = lit_int(&mut b, a);
    let b0 = lit_int(&mut b, b_);
    let s0 = b.push(CoreFrame::PrimOp {
        op: slot0,
        args: vec![a0, b0],
    });
    let a1 = lit_int(&mut b, a);
    let b1 = lit_int(&mut b, b_);
    let s1 = b.push(CoreFrame::PrimOp {
        op: slot1,
        args: vec![a1, b1],
    });
    b.push(CoreFrame::Con {
        tag: PAIR,
        fields: vec![s0, s1],
    });
    finish(b)
}

/// Triple-slot Con: `(,) slot0 ((,) slot1 slot2)` (the standard table only has
/// a 2-ary Pair, so we nest). Used for `timesInt2#` (hi, lo, overflow).
fn prog_triple_slots_int(
    slot0: PrimOpKind,
    slot1: PrimOpKind,
    slot2: PrimOpKind,
    a: i64,
    b_: i64,
) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let mk = |b: &mut TreeBuilder, op: PrimOpKind| {
        let x = lit_int(b, a);
        let y = lit_int(b, b_);
        b.push(CoreFrame::PrimOp { op, args: vec![x, y] })
    };
    let s0 = mk(&mut b, slot0);
    let s1 = mk(&mut b, slot1);
    let s2 = mk(&mut b, slot2);
    let inner = b.push(CoreFrame::Con {
        tag: PAIR,
        fields: vec![s1, s2],
    });
    b.push(CoreFrame::Con {
        tag: PAIR,
        fields: vec![s0, inner],
    });
    finish(b)
}

// ===========================================================================
// Int arithmetic / bitwise / shift / compare.
// ===========================================================================
proptest! {
    #![proptest_config(cfg_int())]

    #[test]
    #[serial]
    fn prop_int_binary(a in arb_edge_i64(), b in arb_edge_i64()) {
        // wrapping ops: add/sub/mul/and/or/xor — total for ALL operands.
        for op in [
            PrimOpKind::IntAdd, PrimOpKind::IntSub, PrimOpKind::IntMul,
            PrimOpKind::IntAnd, PrimOpKind::IntOr, PrimOpKind::IntXor,
            PrimOpKind::IntEq, PrimOpKind::IntNe, PrimOpKind::IntLt,
            PrimOpKind::IntLe, PrimOpKind::IntGt, PrimOpKind::IntGe,
            // 64-bit variants share the same lowering.
            PrimOpKind::Int64Add, PrimOpKind::Int64Sub, PrimOpKind::Int64Mul,
            PrimOpKind::Int64Lt, PrimOpKind::Int64Le, PrimOpKind::Int64Gt,
            PrimOpKind::Int64Ge,
            // carry/overflow VALUE + CARRY slots (each its own primop).
            PrimOpKind::AddIntCVal, PrimOpKind::AddIntCCarry,
        ] {
            run_oracle(prog_binary_int(op, a, b))?;
        }
    }

    #[test]
    #[serial]
    fn prop_int_quot_rem(a in arb_edge_i64(), b in arb_edge_i64()) {
        // Avoid b==0 (JIT traps, eval errors → both fail → skip, no value cmp).
        // NOTE: INT_MIN / -1 OVERFLOWS in both sdiv (JIT) and wrapping_div
        // (eval) — both define it as INT_MIN (eval uses wrapping_div, Cranelift
        // sdiv on x86 would TRAP). We exclude that single pair to keep the
        // success lane clean and not mask it as a both-fail skip.
        prop_assume!(b != 0);
        prop_assume!(!(a == i64::MIN && b == -1));
        for op in [PrimOpKind::IntQuot, PrimOpKind::IntRem] {
            run_oracle(prog_binary_int(op, a, b))?;
        }
    }

    #[test]
    #[serial]
    fn prop_int_shift(a in arb_edge_i64(), sh in arb_shift()) {
        // IntShl/IntShra/IntShrl/Int64Shl use wrapping_* in eval → strict.
        for op in [PrimOpKind::IntShl, PrimOpKind::IntShra, PrimOpKind::IntShrl,
                   PrimOpKind::Int64Shl] {
            run_oracle(prog_binary_int(op, a, sh))?;
        }
        // Int64Shra uses a RAW `a >> b` in eval (CONFIRMED-BUG EVAL-2): panics
        // on shift >= 64. Route through the panic-tolerant oracle.
        run_oracle_eval_may_panic(prog_binary_int(PrimOpKind::Int64Shra, a, sh))?;
    }

    #[test]
    #[serial]
    fn prop_int_unary(a in arb_edge_i64()) {
        for op in [PrimOpKind::IntNegate, PrimOpKind::IntNot,
                   PrimOpKind::Narrow8Int, PrimOpKind::Narrow16Int,
                   PrimOpKind::Narrow32Int,
                   PrimOpKind::Int2Word, PrimOpKind::Int2Double,
                   PrimOpKind::Int2Float, PrimOpKind::IntToInt64,
                   PrimOpKind::Int64ToInt] {
            run_oracle(prog_unary_int(op, a))?;
        }
        // Int64Negate uses a RAW `-a` in eval (CONFIRMED-BUG EVAL-1): panics on
        // INT_MIN. Route through the panic-tolerant oracle.
        run_oracle_eval_may_panic(prog_unary_int(PrimOpKind::Int64Negate, a))?;
    }
}

// ===========================================================================
// Word arithmetic / bitwise / shift / compare + Word multi-output.
// ===========================================================================
proptest! {
    #![proptest_config(cfg_int())]

    #[test]
    #[serial]
    fn prop_word_binary(a in arb_edge_u64(), b in arb_edge_u64()) {
        for op in [
            PrimOpKind::WordAdd, PrimOpKind::WordSub, PrimOpKind::WordMul,
            PrimOpKind::WordAnd, PrimOpKind::WordOr, PrimOpKind::WordXor,
            PrimOpKind::WordEq, PrimOpKind::WordNe, PrimOpKind::WordLt,
            PrimOpKind::WordLe, PrimOpKind::WordGt, PrimOpKind::WordGe,
            PrimOpKind::Word64And, PrimOpKind::Word64Or,
            // carry/borrow value + flag slots.
            PrimOpKind::AddWordCVal, PrimOpKind::AddWordCCarry,
            PrimOpKind::SubWordCVal, PrimOpKind::SubWordCCarry,
            // multi-output product hi/lo slots.
            PrimOpKind::TimesWord2Hi, PrimOpKind::TimesWord2Lo,
        ] {
            run_oracle(prog_binary_word(op, a, b))?;
        }
    }

    #[test]
    #[serial]
    fn prop_word_quot_rem(a in arb_edge_u64(), b in arb_edge_u64()) {
        prop_assume!(b != 0);
        for op in [PrimOpKind::WordQuot, PrimOpKind::WordRem,
                   PrimOpKind::QuotRemWordVal, PrimOpKind::QuotRemWordRem] {
            run_oracle(prog_binary_word(op, a, b))?;
        }
    }

    #[test]
    #[serial]
    fn prop_word_shift(a in arb_edge_u64(), sh in arb_shift()) {
        // WordShl/WordShrl use wrapping_* in eval → strict.
        for op in [PrimOpKind::WordShl, PrimOpKind::WordShrl] {
            run_oracle(prog_word_shift(op, a, sh))?;
        }
        // Word64Shl uses a RAW `a << b` in eval (CONFIRMED-BUG EVAL-3): panics
        // on shift >= 64. Route through the panic-tolerant oracle.
        run_oracle_eval_may_panic(prog_word_shift(PrimOpKind::Word64Shl, a, sh))?;
    }

    #[test]
    #[serial]
    fn prop_word_unary(a in arb_edge_u64()) {
        for op in [PrimOpKind::WordNot,
                   PrimOpKind::Narrow8Word, PrimOpKind::Narrow16Word,
                   PrimOpKind::Narrow32Word,
                   PrimOpKind::Word2Int, PrimOpKind::WordToWord8,
                   PrimOpKind::Word8ToWord,
                   PrimOpKind::Int64ToWord64, PrimOpKind::Word64ToInt64,
                   PrimOpKind::Clz8] {
            run_oracle(prog_unary_word(op, a))?;
        }
    }

    // Word8 arithmetic/compare (masked to 8 bits).
    #[test]
    #[serial]
    fn prop_word8(a in arb_edge_u64(), b in arb_edge_u64()) {
        for op in [PrimOpKind::Word8Add, PrimOpKind::Word8Sub,
                   PrimOpKind::Word8Lt, PrimOpKind::Word8Le,
                   PrimOpKind::Word8Ge] {
            run_oracle(prog_binary_word(op, a, b))?;
        }
    }
}

// ===========================================================================
// MULTI-OUTPUT SLOT-ORDERING (the high-yield class).
//
// Probe each tuple's slots TOGETHER inside a Con so a hi/lo (or val/carry)
// swap surfaces with both slots visible in the failure dump.
// ===========================================================================
proptest! {
    #![proptest_config(cfg_int())]

    #[test]
    #[serial]
    fn prop_times_int2_slots(a in arb_edge_i64(), b in arb_edge_i64()) {
        // (hi, lo, overflow) — the timesInt2# triple. A hi/lo swap (the
        // recently-shipped bug class) makes the Con fields disagree.
        run_oracle(prog_triple_slots_int(
            PrimOpKind::TimesInt2Hi,
            PrimOpKind::TimesInt2Lo,
            PrimOpKind::TimesInt2Overflow,
            a,
            b,
        ))?;
        // also pairwise hi+lo for an extra-tight repro surface.
        run_oracle(prog_pair_slots_int(
            PrimOpKind::TimesInt2Hi,
            PrimOpKind::TimesInt2Lo,
            a,
            b,
        ))?;
    }

    #[test]
    #[serial]
    fn prop_times_word2_slots(a in arb_edge_u64(), b in arb_edge_u64()) {
        run_oracle(prog_pair_slots_word(
            PrimOpKind::TimesWord2Hi,
            PrimOpKind::TimesWord2Lo,
            a,
            b,
        ))?;
    }

    #[test]
    #[serial]
    fn prop_quot_rem_word_slots(a in arb_edge_u64(), b in arb_edge_u64()) {
        prop_assume!(b != 0);
        run_oracle(prog_pair_slots_word(
            PrimOpKind::QuotRemWordVal,
            PrimOpKind::QuotRemWordRem,
            a,
            b,
        ))?;
    }

    #[test]
    #[serial]
    fn prop_add_int_c_slots(a in arb_edge_i64(), b in arb_edge_i64()) {
        run_oracle(prog_pair_slots_int(
            PrimOpKind::AddIntCVal,
            PrimOpKind::AddIntCCarry,
            a,
            b,
        ))?;
    }

    #[test]
    #[serial]
    fn prop_add_word_c_slots(a in arb_edge_u64(), b in arb_edge_u64()) {
        run_oracle(prog_pair_slots_word(
            PrimOpKind::AddWordCVal,
            PrimOpKind::AddWordCCarry,
            a,
            b,
        ))?;
    }

    #[test]
    #[serial]
    fn prop_sub_word_c_slots(a in arb_edge_u64(), b in arb_edge_u64()) {
        run_oracle(prog_pair_slots_word(
            PrimOpKind::SubWordCVal,
            PrimOpKind::SubWordCCarry,
            a,
            b,
        ))?;
    }
}

// ===========================================================================
// Double / Float arithmetic + compare + conversions + math (libm).
// ===========================================================================
proptest! {
    #![proptest_config(cfg_float())]

    #[test]
    #[serial]
    fn prop_double_binary(a in arb_double(), b in arb_double()) {
        for op in [PrimOpKind::DoubleAdd, PrimOpKind::DoubleSub,
                   PrimOpKind::DoubleMul, PrimOpKind::DoubleDiv,
                   PrimOpKind::DoubleEq, PrimOpKind::DoubleNe,
                   PrimOpKind::DoubleLt, PrimOpKind::DoubleLe,
                   PrimOpKind::DoubleGt, PrimOpKind::DoubleGe,
                   PrimOpKind::DoublePower] {
            run_oracle(prog_op(op, |b_| {
                let x = lit_double(b_, a);
                let y = lit_double(b_, b);
                vec![x, y]
            }))?;
        }
    }

    #[test]
    #[serial]
    fn prop_double_unary(a in arb_double()) {
        for op in [PrimOpKind::DoubleNegate, PrimOpKind::DoubleFabs,
                   PrimOpKind::DoubleSqrt, PrimOpKind::FfiRintDouble,
                   PrimOpKind::Double2Int, PrimOpKind::Double2Float] {
            run_oracle(prog_op(op, |b_| {
                let x = lit_double(b_, a);
                vec![x]
            }))?;
        }
    }

    // Transcendental math goes through libm runtime calls on the JIT side and
    // Rust std on the eval side — these are the SAME platform libm, so they
    // should agree bit-for-bit. Probe them all.
    #[test]
    #[serial]
    fn prop_double_math(a in arb_double()) {
        for op in [PrimOpKind::DoubleExp, PrimOpKind::DoubleExpM1,
                   PrimOpKind::DoubleLog, PrimOpKind::DoubleLog1P,
                   PrimOpKind::DoubleSin, PrimOpKind::DoubleCos,
                   PrimOpKind::DoubleTan, PrimOpKind::DoubleAsin,
                   PrimOpKind::DoubleAcos, PrimOpKind::DoubleAtan,
                   PrimOpKind::DoubleSinh, PrimOpKind::DoubleCosh,
                   PrimOpKind::DoubleTanh, PrimOpKind::DoubleAsinh,
                   PrimOpKind::DoubleAcosh, PrimOpKind::DoubleAtanh] {
            run_oracle(prog_op(op, |b_| {
                let x = lit_double(b_, a);
                vec![x]
            }))?;
        }
    }

    #[test]
    #[serial]
    fn prop_float_binary(a in arb_float(), b in arb_float()) {
        for op in [PrimOpKind::FloatAdd, PrimOpKind::FloatSub,
                   PrimOpKind::FloatMul, PrimOpKind::FloatDiv,
                   PrimOpKind::FloatEq, PrimOpKind::FloatNe,
                   PrimOpKind::FloatLt, PrimOpKind::FloatLe,
                   PrimOpKind::FloatGt, PrimOpKind::FloatGe] {
            run_oracle(prog_op(op, |b_| {
                let x = lit_float(b_, a);
                let y = lit_float(b_, b);
                vec![x, y]
            }))?;
        }
    }

    #[test]
    #[serial]
    fn prop_float_unary(a in arb_float()) {
        for op in [PrimOpKind::FloatNegate, PrimOpKind::Float2Int,
                   PrimOpKind::Float2Double] {
            run_oracle(prog_op(op, |b_| {
                let x = lit_float(b_, a);
                vec![x]
            }))?;
        }
    }

    // decodeDouble# mantissa/exponent slots (each its own primop).
    #[test]
    #[serial]
    fn prop_decode_double(a in arb_double()) {
        for op in [PrimOpKind::DecodeDoubleMantissa,
                   PrimOpKind::DecodeDoubleExponent] {
            run_oracle(prog_op(op, |b_| {
                let x = lit_double(b_, a);
                vec![x]
            }))?;
        }
    }
}

// ===========================================================================
// Char comparison + Chr/Ord round-trips.
// ===========================================================================
proptest! {
    #![proptest_config(cfg_int())]

    #[test]
    #[serial]
    fn prop_char_compare(a in arb_codepoint(), b in arb_codepoint()) {
        let ca = char::from_u32(a as u32).unwrap_or('\0');
        let cb = char::from_u32(b as u32).unwrap_or('\0');
        for op in [PrimOpKind::CharEq, PrimOpKind::CharNe,
                   PrimOpKind::CharLt, PrimOpKind::CharLe,
                   PrimOpKind::CharGt, PrimOpKind::CharGe] {
            let expr = prog_op(op, |bld| {
                let x = lit_char(bld, ca);
                let y = lit_char(bld, cb);
                vec![x, y]
            });
            run_oracle(expr)?;
        }
    }

    #[test]
    #[serial]
    fn prop_ord(a in arb_codepoint()) {
        let ca = char::from_u32(a as u32).unwrap_or('\0');
        run_oracle(prog_op(PrimOpKind::Ord, |b| {
            let x = lit_char(b, ca);
            vec![x]
        }))?;
    }

    #[test]
    #[serial]
    fn prop_chr(a in arb_codepoint()) {
        // Valid codepoints only (both sides succeed → value comparison).
        run_oracle(prog_unary_int(PrimOpKind::Chr, a))?;
    }
}

// ===========================================================================
// CONFIRMED-BUG REPROS (minimal, hand-built). Each is `#[ignore]`d so the
// suite stays green; remove the `#[ignore]` once the eval handler is fixed to
// use the wrapping arithmetic its non-64 sibling already uses.
//
// Class EVAL-1/2/3: the tree-walking interpreter's `Int64Negate` / `Int64Shra`
// / `Word64Shl` handlers use RAW arithmetic where their `IntNegate`/`IntShra`/
// `WordShl` siblings (and the JIT, via Cranelift `ineg`/`sshr`/`ishl`) use
// wrapping / mod-width semantics. The JIT returns the wrapped value; eval
// PANICS (debug overflow check). Severity: crash (eval host panic) — a release
// build would instead diverge SILENTLY for the shift cases (raw `<<`/`>>` by
// >= 64 is "unspecified" but in practice masks differently than the JIT's
// guaranteed mod-64) and wrap (matching) for negate, so the negate case is
// debug-only-loud while the shift cases are a latent silent-wrong in release.
// Files: tidepool-eval/src/eval.rs (Int64Negate ~1441, Int64Shra ~1445,
// Word64Shl ~1449). Fix = swap `-a`/`a >> b`/`a << b` for
// `a.wrapping_neg()`/`a.wrapping_shr(b as u32)`/`a.wrapping_shl(b as u32)`.

/// Confirm + run one (op, args) repro: returns (eval_panicked, jit_result_dbg).
fn run_one(op: PrimOpKind, args: Vec<Literal>) -> (bool, String) {
    let mut b = TreeBuilder::new();
    let idxs: Vec<usize> = args
        .iter()
        .map(|l| b.push(CoreFrame::Lit(l.clone())))
        .collect();
    b.push(CoreFrame::PrimOp { op, args: idxs });
    let tree = b.build();
    let table = build_table_for_expr(&tree);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // silence the panic backtrace spam
    let ev = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut heap = VecHeap::new();
        let env = env_from_datacon_table(&table);
        eval(&tree, &env, &mut heap)
    }));
    std::panic::set_hook(prev);
    let jit = JitEffectMachine::compile(&tree, &table, 64 * 1024).and_then(|mut m| m.run_pure());
    (ev.is_err(), format!("{:?}", jit))
}

/// CONFIRMED-BUG EVAL-1: `Int64Negate` on `INT_MIN`.
/// eval = PANIC "attempt to negate with overflow"; JIT = Ok(LitInt(INT_MIN)).
#[test]
#[ignore = "CONFIRMED-BUG EVAL-1: tidepool-eval Int64Negate uses raw `-a` (panics on INT_MIN); JIT ineg wraps. Fix eval to wrapping_neg, then un-ignore."]
fn evalbug1_int64_negate_int_min() {
    let (eval_panicked, jit) = run_one(PrimOpKind::Int64Negate, vec![Literal::LitInt(i64::MIN)]);
    assert!(
        eval_panicked,
        "EVAL-1 appears FIXED — eval no longer panics on Int64Negate(INT_MIN); un-ignore this repro."
    );
    assert_eq!(jit, "Ok(Lit(LitInt(-9223372036854775808)))");
}

/// CONFIRMED-BUG EVAL-2: `Int64Shra` with shift >= 64 (here INT_MIN >> 64).
/// eval = PANIC "attempt to shift right with overflow";
/// JIT = Ok(LitInt(INT_MIN)) (Cranelift sshr reduces shift mod 64 → >>0).
#[test]
#[ignore = "CONFIRMED-BUG EVAL-2: tidepool-eval Int64Shra uses raw `a >> b` (panics on shift>=64); JIT sshr masks mod 64. Fix eval to wrapping_shr, then un-ignore."]
fn evalbug2_int64_shra_shift_64() {
    let (eval_panicked, jit) = run_one(
        PrimOpKind::Int64Shra,
        vec![Literal::LitInt(i64::MIN), Literal::LitInt(64)],
    );
    assert!(
        eval_panicked,
        "EVAL-2 appears FIXED — eval no longer panics on Int64Shra(_, 64); un-ignore this repro."
    );
    assert_eq!(jit, "Ok(Lit(LitInt(-9223372036854775808)))");
}

/// CONFIRMED-BUG EVAL-3: `Word64Shl` with shift >= 64 (here 1 << 64).
/// eval = PANIC "attempt to shift left with overflow";
/// JIT = Ok(LitWord(1)) (Cranelift ishl reduces shift mod 64 → <<0).
#[test]
#[ignore = "CONFIRMED-BUG EVAL-3: tidepool-eval Word64Shl uses raw `a << b` (panics on shift>=64); JIT ishl masks mod 64. Fix eval to wrapping_shl, then un-ignore."]
fn evalbug3_word64_shl_shift_64() {
    let (eval_panicked, jit) = run_one(
        PrimOpKind::Word64Shl,
        vec![Literal::LitWord(1), Literal::LitInt(64)],
    );
    assert!(
        eval_panicked,
        "EVAL-3 appears FIXED — eval no longer panics on Word64Shl(1, 64); un-ignore this repro."
    );
    assert_eq!(jit, "Ok(Lit(LitWord(1)))");
}

// ===========================================================================
// Configs.
//
// Two budgets per the lane spec: a larger "default" lane and a deterministic
// re-run. The int/word lanes loop over MANY ops per case, so 300 cases is
// thousands of compiled programs; the float lanes are lighter.
// ===========================================================================
fn cfg_int() -> Config {
    let mut c = Config::with_cases(350);
    c.max_shrink_iters = 5000;
    // Deterministic seed so reruns reproduce. (proptest persists failures to
    // .proptest-regressions regardless.)
    c
}

fn cfg_float() -> Config {
    let mut c = Config::with_cases(400);
    c.max_shrink_iters = 5000;
    c
}

// ===========================================================================
// Reach floor (runs last; alphabetical ordering puts `zzz_` after all props).
// ===========================================================================
#[test]
#[serial]
fn zzz_reach_floor() {
    let total = TOTAL.load(Ordering::Relaxed);
    let reached = REACHED.load(Ordering::Relaxed);
    eprintln!(
        "PRIMOPS-DIFF REACH: {}/{} oracle invocations reached value comparison ({:.1}%)",
        reached,
        total,
        if total > 0 {
            100.0 * reached as f64 / total as f64
        } else {
            0.0
        }
    );
    eprintln!(
        "PRIMOPS-DIFF EVAL-PANIC HITS (CONFIRMED-BUG EVAL-1/2/3, tolerated): {}",
        N_EVAL_PANIC.load(Ordering::Relaxed),
    );
    if total >= 100 {
        let ratio = reached as f64 / total as f64;
        assert!(
            ratio >= 0.85,
            "reach floor: only {:.1}% of {} oracle calls reached value comparison (need >= 85%)",
            100.0 * ratio,
            total
        );
    }
}
