//! Lane: EXHAUSTIVE PRIMOPS differential (JIT vs tree-walking eval).
//!
//! Goal: drive EVERY `PrimOpKind` the JIT supports through the classified
//! `tidepool_testing::differential` runner with EDGE operands (INT_MIN/MAX,
//! WORD_MAX, 0, +/-1, small negatives, sign-bit patterns), and surface any
//! JIT-vs-eval divergence with a minimal repro.
//!
//! This is a SELF-CONTAINED lane: it adds its own generators (it does not edit
//! the shared `strategy.rs`), and drives the shared [`DiffConfig`]/[`check`]
//! oracle from `tidepool-testing` at two nursery sizes (64 KiB + 16 KiB — these
//! are tiny scalar programs so a 16 KiB nursery is more than enough; the second
//! size exercises the nursery-knob as a runtime-configuration invariant).
//!
//! ## Tolerated-class policy: NOTHING
//!
//! Every property in this lane generates TOTAL, GROUND programs: the only
//! partiality (`b == 0`, `i64::MIN / -1`) is excluded from the generator's
//! DOMAIN via `prop_assume!`, not tolerated as an outcome. So the policy is
//! empty — an eval error, a JIT-only error, or a nursery-size disagreement is
//! always a failure here, never a discard.
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
//! `TimesInt2Overflow`, etc.). A slot-ordering bug (such as a `timesInt2#`
//! hi/lo swap) manifests as a wrong value in exactly one slot. We
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
//!
//! ## Reach accounting
//!
//! Each property drives `proptest::test_runner::TestRunner` directly (not the
//! `proptest!` macro) so it can own a local [`ReachCounter`] and assert its
//! floor in the SAME process as the cases that fed it — a `zzz_`-ordered floor
//! test reading a `static` populated by separate `#[test]` fns would always see
//! zero under nextest's process-per-test isolation.

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serial_test::serial;

use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::types::{DataConId, Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_testing::differential::{check, DiffConfig, ReachCounter};
use tidepool_testing::proptest::build_table_for_expr;

use tidepool_codegen::jit_machine::JitEffectMachine;

// Standard-table DataCon ids (must match `standard_datacon_table`).
const PAIR: DataConId = DataConId(4);

// ---------------------------------------------------------------------------
// The oracle: two nursery sizes, nothing tolerated (see module docs).
// ---------------------------------------------------------------------------
fn dcfg() -> DiffConfig {
    DiffConfig::new("primops").nurseries(&[64 * 1024, 16 * 1024])
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
        0x7FFF_FFFF,               // i32::MAX
        -0x8000_0000,              // i32::MIN
        0x1_0000_0000,             // 2^32
        0x7FFF,                    // i16::MAX
        -0x8000,                   // i16::MIN
        0x7F,                      // i8::MAX
        -0x80,                     // i8::MIN
        0x5555_5555_5555_5555,     // alternating bits
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
        Just(0x41i64), // 'A'
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
        b.push(CoreFrame::PrimOp {
            op,
            args: vec![x, y],
        })
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

#[test]
#[serial]
fn prop_int_binary() {
    let reach = ReachCounter::new("primops/int_binary");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_i64(), arb_edge_i64()), |(a, b)| {
            // wrapping ops: add/sub/mul/and/or/xor — total for ALL operands.
            for op in [
                PrimOpKind::IntAdd,
                PrimOpKind::IntSub,
                PrimOpKind::IntMul,
                PrimOpKind::IntAnd,
                PrimOpKind::IntOr,
                PrimOpKind::IntXor,
                PrimOpKind::IntEq,
                PrimOpKind::IntNe,
                PrimOpKind::IntLt,
                PrimOpKind::IntLe,
                PrimOpKind::IntGt,
                PrimOpKind::IntGe,
                // 64-bit variants share the same lowering.
                PrimOpKind::Int64Add,
                PrimOpKind::Int64Sub,
                PrimOpKind::Int64Mul,
                PrimOpKind::Int64Eq,
                PrimOpKind::Int64Ne,
                PrimOpKind::Int64Lt,
                PrimOpKind::Int64Le,
                PrimOpKind::Int64Gt,
                PrimOpKind::Int64Ge,
                // carry/overflow VALUE + CARRY slots (each its own primop).
                PrimOpKind::AddIntCVal,
                PrimOpKind::AddIntCCarry,
            ] {
                check(prog_binary_int(op, a, b), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_int_quot_rem() {
    let reach = ReachCounter::new("primops/int_quot_rem");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_i64(), arb_edge_i64()), |(a, b)| {
            // Avoid b==0 (JIT traps, eval errors → both fail → skip, no value cmp).
            // NOTE: INT_MIN / -1 OVERFLOWS in both sdiv (JIT) and wrapping_div
            // (eval) — both define it as INT_MIN (eval uses wrapping_div, Cranelift
            // sdiv on x86 would TRAP). We exclude that single pair to keep the
            // success lane clean and not mask it as a both-fail skip.
            prop_assume!(b != 0);
            prop_assume!(!(a == i64::MIN && b == -1));
            for op in [
                PrimOpKind::IntQuot,
                PrimOpKind::IntRem,
                PrimOpKind::Int64Quot,
                PrimOpKind::Int64Rem,
            ] {
                check(prog_binary_int(op, a, b), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_int_shift() {
    let reach = ReachCounter::new("primops/int_shift");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_i64(), arb_shift()), |(a, sh)| {
            // IntShl/IntShra/IntShrl/Int64Shl use wrapping_* in eval → strict.
            for op in [
                PrimOpKind::IntShl,
                PrimOpKind::IntShra,
                PrimOpKind::IntShrl,
                PrimOpKind::Int64Shl,
                PrimOpKind::Int64Shrl,
            ] {
                check(prog_binary_int(op, a, sh), &dcfg(), &reach)?;
            }
            // Int64Shra uses wrapping_shr in eval (fixed, pinned by evalbug2 below) → strict.
            check(
                prog_binary_int(PrimOpKind::Int64Shra, a, sh),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_int_unary() {
    let reach = ReachCounter::new("primops/int_unary");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&arb_edge_i64(), |a| {
            for op in [
                PrimOpKind::IntNegate,
                PrimOpKind::IntNot,
                PrimOpKind::Narrow8Int,
                PrimOpKind::Narrow16Int,
                PrimOpKind::Narrow32Int,
                PrimOpKind::Int2Word,
                PrimOpKind::Int2Double,
                PrimOpKind::Int2Float,
                PrimOpKind::IntToInt64,
                PrimOpKind::Int64ToInt,
                PrimOpKind::Int64ToWord64,
            ] {
                check(prog_unary_int(op, a), &dcfg(), &reach)?;
            }
            // Int64Negate uses wrapping_neg in eval (fixed, pinned by evalbug1 below) → strict.
            check(prog_unary_int(PrimOpKind::Int64Negate, a), &dcfg(), &reach)?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// ===========================================================================
// Word arithmetic / bitwise / shift / compare + Word multi-output.
// ===========================================================================

#[test]
#[serial]
fn prop_word_binary() {
    let reach = ReachCounter::new("primops/word_binary");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            for op in [
                PrimOpKind::WordAdd,
                PrimOpKind::WordSub,
                PrimOpKind::WordMul,
                PrimOpKind::WordAnd,
                PrimOpKind::WordOr,
                PrimOpKind::WordXor,
                PrimOpKind::WordEq,
                PrimOpKind::WordNe,
                PrimOpKind::WordLt,
                PrimOpKind::WordLe,
                PrimOpKind::WordGt,
                PrimOpKind::WordGe,
                PrimOpKind::Word64And,
                PrimOpKind::Word64Or,
                PrimOpKind::Word64Add,
                PrimOpKind::Word64Sub,
                PrimOpKind::Word64Mul,
                PrimOpKind::Word64Xor,
                // carry/borrow value + flag slots.
                PrimOpKind::AddWordCVal,
                PrimOpKind::AddWordCCarry,
                PrimOpKind::SubWordCVal,
                PrimOpKind::SubWordCCarry,
                // multi-output product hi/lo slots.
                PrimOpKind::TimesWord2Hi,
                PrimOpKind::TimesWord2Lo,
            ] {
                check(prog_binary_word(op, a, b), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_word_quot_rem() {
    let reach = ReachCounter::new("primops/word_quot_rem");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            prop_assume!(b != 0);
            for op in [
                PrimOpKind::WordQuot,
                PrimOpKind::WordRem,
                PrimOpKind::Word64Quot,
                PrimOpKind::Word64Rem,
                PrimOpKind::QuotRemWordVal,
                PrimOpKind::QuotRemWordRem,
            ] {
                check(prog_binary_word(op, a, b), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_word_shift() {
    let reach = ReachCounter::new("primops/word_shift");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_shift()), |(a, sh)| {
            // WordShl/WordShrl/Word64Shl/Word64Shrl all use wrapping_* in eval → strict.
            for op in [
                PrimOpKind::WordShl,
                PrimOpKind::WordShrl,
                PrimOpKind::Word64Shl,
                PrimOpKind::Word64Shrl,
            ] {
                check(prog_word_shift(op, a, sh), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_word_unary() {
    let reach = ReachCounter::new("primops/word_unary");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&arb_edge_u64(), |a| {
            for op in [
                PrimOpKind::WordNot,
                PrimOpKind::Word64Not,
                PrimOpKind::Narrow8Word,
                PrimOpKind::Narrow16Word,
                PrimOpKind::Narrow32Word,
                PrimOpKind::Word2Int,
                PrimOpKind::WordToWord8,
                PrimOpKind::Word8ToWord,
                PrimOpKind::Word64ToInt64,
                PrimOpKind::Clz8,
            ] {
                check(prog_unary_word(op, a), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// Word8 arithmetic/compare (masked to 8 bits).
#[test]
#[serial]
fn prop_word8() {
    let reach = ReachCounter::new("primops/word8");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            for op in [
                PrimOpKind::Word8Add,
                PrimOpKind::Word8Sub,
                PrimOpKind::Word8Lt,
                PrimOpKind::Word8Le,
                PrimOpKind::Word8Ge,
            ] {
                check(prog_binary_word(op, a, b), &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// ===========================================================================
// MULTI-OUTPUT SLOT-ORDERING (the high-yield class).
//
// Probe each tuple's slots TOGETHER inside a Con so a hi/lo (or val/carry)
// swap surfaces with both slots visible in the failure dump.
// ===========================================================================

#[test]
#[serial]
fn prop_times_int2_slots() {
    let reach = ReachCounter::new("primops/times_int2_slots");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_i64(), arb_edge_i64()), |(a, b)| {
            // (hi, lo, overflow) — the timesInt2# triple. A hi/lo swap makes
            // the Con fields disagree.
            check(
                prog_triple_slots_int(
                    PrimOpKind::TimesInt2Hi,
                    PrimOpKind::TimesInt2Lo,
                    PrimOpKind::TimesInt2Overflow,
                    a,
                    b,
                ),
                &dcfg(),
                &reach,
            )?;
            // also pairwise hi+lo for an extra-tight repro surface.
            check(
                prog_pair_slots_int(PrimOpKind::TimesInt2Hi, PrimOpKind::TimesInt2Lo, a, b),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_times_word2_slots() {
    let reach = ReachCounter::new("primops/times_word2_slots");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            check(
                prog_pair_slots_word(PrimOpKind::TimesWord2Hi, PrimOpKind::TimesWord2Lo, a, b),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_quot_rem_word_slots() {
    let reach = ReachCounter::new("primops/quot_rem_word_slots");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            prop_assume!(b != 0);
            check(
                prog_pair_slots_word(PrimOpKind::QuotRemWordVal, PrimOpKind::QuotRemWordRem, a, b),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_add_int_c_slots() {
    let reach = ReachCounter::new("primops/add_int_c_slots");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_i64(), arb_edge_i64()), |(a, b)| {
            check(
                prog_pair_slots_int(PrimOpKind::AddIntCVal, PrimOpKind::AddIntCCarry, a, b),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_add_word_c_slots() {
    let reach = ReachCounter::new("primops/add_word_c_slots");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            check(
                prog_pair_slots_word(PrimOpKind::AddWordCVal, PrimOpKind::AddWordCCarry, a, b),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_sub_word_c_slots() {
    let reach = ReachCounter::new("primops/sub_word_c_slots");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_edge_u64(), arb_edge_u64()), |(a, b)| {
            check(
                prog_pair_slots_word(PrimOpKind::SubWordCVal, PrimOpKind::SubWordCCarry, a, b),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// ===========================================================================
// Double / Float arithmetic + compare + conversions + math (libm).
// ===========================================================================

#[test]
#[serial]
fn prop_double_binary() {
    let reach = ReachCounter::new("primops/double_binary");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&(arb_double(), arb_double()), |(a, b)| {
            for op in [
                PrimOpKind::DoubleAdd,
                PrimOpKind::DoubleSub,
                PrimOpKind::DoubleMul,
                PrimOpKind::DoubleDiv,
                PrimOpKind::DoubleEq,
                PrimOpKind::DoubleNe,
                PrimOpKind::DoubleLt,
                PrimOpKind::DoubleLe,
                PrimOpKind::DoubleGt,
                PrimOpKind::DoubleGe,
                PrimOpKind::DoublePower,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_double(b_, a);
                        let y = lit_double(b_, b);
                        vec![x, y]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_double_unary() {
    let reach = ReachCounter::new("primops/double_unary");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_double(), |a| {
            for op in [
                PrimOpKind::DoubleNegate,
                PrimOpKind::DoubleFabs,
                PrimOpKind::DoubleSqrt,
                PrimOpKind::FfiRintDouble,
                PrimOpKind::Double2Int,
                PrimOpKind::Double2Float,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_double(b_, a);
                        vec![x]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// Transcendental math goes through libm runtime calls on the JIT side and
// Rust std on the eval side — these are the SAME platform libm, so they
// should agree bit-for-bit. Probe them all.
#[test]
#[serial]
fn prop_double_math() {
    let reach = ReachCounter::new("primops/double_math");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_double(), |a| {
            for op in [
                PrimOpKind::DoubleExp,
                PrimOpKind::DoubleExpM1,
                PrimOpKind::DoubleLog,
                PrimOpKind::DoubleLog1P,
                PrimOpKind::DoubleSin,
                PrimOpKind::DoubleCos,
                PrimOpKind::DoubleTan,
                PrimOpKind::DoubleAsin,
                PrimOpKind::DoubleAcos,
                PrimOpKind::DoubleAtan,
                PrimOpKind::DoubleSinh,
                PrimOpKind::DoubleCosh,
                PrimOpKind::DoubleTanh,
                PrimOpKind::DoubleAsinh,
                PrimOpKind::DoubleAcosh,
                PrimOpKind::DoubleAtanh,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_double(b_, a);
                        vec![x]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_float_binary() {
    let reach = ReachCounter::new("primops/float_binary");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&(arb_float(), arb_float()), |(a, b)| {
            for op in [
                PrimOpKind::FloatAdd,
                PrimOpKind::FloatSub,
                PrimOpKind::FloatMul,
                PrimOpKind::FloatDiv,
                PrimOpKind::FloatEq,
                PrimOpKind::FloatNe,
                PrimOpKind::FloatLt,
                PrimOpKind::FloatLe,
                PrimOpKind::FloatGt,
                PrimOpKind::FloatGe,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_float(b_, a);
                        let y = lit_float(b_, b);
                        vec![x, y]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_float_unary() {
    let reach = ReachCounter::new("primops/float_unary");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_float(), |a| {
            for op in [
                PrimOpKind::FloatNegate,
                PrimOpKind::Float2Int,
                PrimOpKind::Float2Double,
                PrimOpKind::FloatSqrt,
                PrimOpKind::FloatFabs,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_float(b_, a);
                        vec![x]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// decodeDouble# mantissa/exponent slots (each its own primop).
#[test]
#[serial]
fn prop_decode_double() {
    let reach = ReachCounter::new("primops/decode_double");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_double(), |a| {
            for op in [
                PrimOpKind::DecodeDoubleMantissa,
                PrimOpKind::DecodeDoubleExponent,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_double(b_, a);
                        vec![x]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// decodeFloat_Int# mantissa/exponent slots (each its own primop). Same shape
// as prop_decode_double, over Float's own IEEE754 single layout.
#[test]
#[serial]
fn prop_decode_float() {
    let reach = ReachCounter::new("primops/decode_float");
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_float(), |a| {
            for op in [
                PrimOpKind::DecodeFloatMantissa,
                PrimOpKind::DecodeFloatExponent,
            ] {
                check(
                    prog_op(op, |b_| {
                        let x = lit_float(b_, a);
                        vec![x]
                    }),
                    &dcfg(),
                    &reach,
                )?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

/// Run a unary primop over one `Literal` through the eval oracle, expecting
/// an `Int#` result.
fn eval_lit_int(op: PrimOpKind, lit: Literal) -> i64 {
    let mut b = TreeBuilder::new();
    let x = b.push(CoreFrame::Lit(lit));
    b.push(CoreFrame::PrimOp { op, args: vec![x] });
    let tree = b.build();
    let table = build_table_for_expr(&tree);
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    match eval(&tree, &env, &mut heap) {
        Ok(Value::Lit(Literal::LitInt(n))) => n,
        other => panic!("eval_lit_int: expected Ok(Lit(LitInt(_))), got {other:?}"),
    }
}

/// Same as [`eval_lit_int`], but through the JIT.
fn jit_lit_int(op: PrimOpKind, lit: Literal) -> i64 {
    let mut b = TreeBuilder::new();
    let x = b.push(CoreFrame::Lit(lit));
    b.push(CoreFrame::PrimOp { op, args: vec![x] });
    let tree = b.build();
    let table = build_table_for_expr(&tree);
    match JitEffectMachine::compile(&tree, &table, 64 * 1024).and_then(|mut m| m.run_pure()) {
        Ok(Value::Lit(Literal::LitInt(n))) => n,
        other => panic!("jit_lit_int: expected Ok(Lit(LitInt(_))), got {other:?}"),
    }
}

// GHC-truth canonical-range property: for a NORMAL, finite, nonzero
// double/float, decodeDouble_Int64#/decodeFloat_Int# must return a mantissa
// in [2^(prec-1), 2^prec) (denormals are exempt — GHC's canonical form does
// not hold there). This reads the actual mantissa each engine returns and
// checks it against the mathematical invariant directly, rather than
// comparing eval against the JIT: both engines shared the trailing-zeros
// reduction bug this property pins against, so their prior agreement was
// structurally blind to it.
#[test]
#[serial]
fn prop_decode_double_canonical_range() {
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_double(), |d| {
            let bits = d.to_bits();
            let raw_exp = (bits >> 52) & 0x7ff;
            prop_assume!(d.is_finite() && d != 0.0 && raw_exp != 0);
            let man_eval = eval_lit_int(PrimOpKind::DecodeDoubleMantissa, Literal::LitDouble(bits));
            let man_jit = jit_lit_int(PrimOpKind::DecodeDoubleMantissa, Literal::LitDouble(bits));
            for (engine, man) in [("eval", man_eval), ("jit", man_jit)] {
                let m = man.unsigned_abs();
                prop_assert!(
                    (1u64 << 52..1u64 << 53).contains(&m),
                    "{engine} decodeDouble_Int64# mantissa {man} for {d:e} not in canonical range [2^52, 2^53)"
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
#[serial]
fn prop_decode_float_canonical_range() {
    let mut runner = TestRunner::new(cfg_float());
    runner
        .run(&arb_float(), |f| {
            let bits = f.to_bits();
            let raw_exp = (bits >> 23) & 0xff;
            prop_assume!(f.is_finite() && f != 0.0 && raw_exp != 0);
            let man_eval =
                eval_lit_int(PrimOpKind::DecodeFloatMantissa, Literal::LitFloat(bits as u64));
            let man_jit =
                jit_lit_int(PrimOpKind::DecodeFloatMantissa, Literal::LitFloat(bits as u64));
            for (engine, man) in [("eval", man_eval), ("jit", man_jit)] {
                let m = man.unsigned_abs();
                prop_assert!(
                    (1u64 << 23..1u64 << 24).contains(&m),
                    "{engine} decodeFloat_Int# mantissa {man} for {f:e} not in canonical range [2^23, 2^24)"
                );
            }
            Ok(())
        })
        .unwrap();
}

// ===========================================================================
// Char comparison + Chr/Ord round-trips.
// ===========================================================================

#[test]
#[serial]
fn prop_char_compare() {
    let reach = ReachCounter::new("primops/char_compare");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&(arb_codepoint(), arb_codepoint()), |(a, b)| {
            let ca = char::from_u32(a as u32).unwrap_or('\0');
            let cb = char::from_u32(b as u32).unwrap_or('\0');
            for op in [
                PrimOpKind::CharEq,
                PrimOpKind::CharNe,
                PrimOpKind::CharLt,
                PrimOpKind::CharLe,
                PrimOpKind::CharGt,
                PrimOpKind::CharGe,
            ] {
                let expr = prog_op(op, |bld| {
                    let x = lit_char(bld, ca);
                    let y = lit_char(bld, cb);
                    vec![x, y]
                });
                check(expr, &dcfg(), &reach)?;
            }
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_ord() {
    let reach = ReachCounter::new("primops/ord");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&arb_codepoint(), |a| {
            let ca = char::from_u32(a as u32).unwrap_or('\0');
            check(
                prog_op(PrimOpKind::Ord, |b| {
                    let x = lit_char(b, ca);
                    vec![x]
                }),
                &dcfg(),
                &reach,
            )?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

#[test]
#[serial]
fn prop_chr() {
    let reach = ReachCounter::new("primops/chr");
    let mut runner = TestRunner::new(cfg_int());
    runner
        .run(&arb_codepoint(), |a| {
            // Valid codepoints only (both sides succeed → value comparison).
            check(prog_unary_int(PrimOpKind::Chr, a), &dcfg(), &reach)?;
            Ok(())
        })
        .unwrap();
    reach.assert_floor(0.85);
}

// ===========================================================================
// FIXED-BUG REPROS (minimal, hand-built): the tree-walking interpreter's
// `Int64Negate` / `Int64Shra` / `Word64Shl` handlers used RAW arithmetic where
// their `IntNegate`/`IntShra`/`WordShl` siblings (and the JIT, via Cranelift
// `ineg`/`sshr`/`ishl`) use wrapping / mod-width semantics — now fixed to
// `wrapping_neg`/`wrapping_shr`/`wrapping_shl`, matching the JIT. `Word64Shrl`
// had the same raw-`>>` gap and is now `wrapping_shr` too. These pin the fix;
// the corresponding lanes above (`prop_int_shift`, `prop_int_unary`,
// `prop_word_shift`) route all four ops through the strict `check`.

/// Confirm + run one (op, args) repro: returns (eval_result_dbg_or_panic, jit_result_dbg).
fn run_one(op: PrimOpKind, args: Vec<Literal>) -> (String, String) {
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
    let eval_dbg = match ev {
        Ok(res) => format!("{:?}", res),
        Err(_) => "PANIC".to_string(),
    };
    (eval_dbg, format!("{:?}", jit))
}

/// `Int64Negate` on `INT_MIN` wraps like the JIT.
#[test]
fn evalbug1_int64_negate_int_min() {
    let (eval, jit) = run_one(PrimOpKind::Int64Negate, vec![Literal::LitInt(i64::MIN)]);
    assert_eq!(jit, "Ok(Lit(LitInt(-9223372036854775808)))");
    assert_eq!(eval, "Ok(Lit(LitInt(-9223372036854775808)))");
}

/// `Int64Shra` with shift >= 64 (here INT_MIN >> 64) masks mod 64 like the
/// JIT's Cranelift `sshr`.
#[test]
fn evalbug2_int64_shra_shift_64() {
    let (eval, jit) = run_one(
        PrimOpKind::Int64Shra,
        vec![Literal::LitInt(i64::MIN), Literal::LitInt(64)],
    );
    assert_eq!(jit, "Ok(Lit(LitInt(-9223372036854775808)))");
    assert_eq!(eval, "Ok(Lit(LitInt(-9223372036854775808)))");
}

/// `Word64Shl` with shift >= 64 (here 1 << 64) masks mod 64 like the JIT's
/// Cranelift `ishl`.
#[test]
fn evalbug3_word64_shl_shift_64() {
    let (eval, jit) = run_one(
        PrimOpKind::Word64Shl,
        vec![Literal::LitWord(1), Literal::LitInt(64)],
    );
    assert_eq!(jit, "Ok(Lit(LitWord(1)))");
    assert_eq!(eval, "Ok(Lit(LitWord(1)))");
}

/// `Word64Shrl` with shift >= 64 (here 1 >> 64) masks mod 64 like the JIT's
/// Cranelift `ushr`, instead of panicking ("shift right with overflow") on the
/// old raw `>>`.
#[test]
fn evalbug4_word64_shrl_shift_64() {
    let (eval, jit) = run_one(
        PrimOpKind::Word64Shrl,
        vec![Literal::LitWord(1), Literal::LitInt(64)],
    );
    assert_eq!(jit, "Ok(Lit(LitWord(1)))");
    assert_eq!(eval, "Ok(Lit(LitWord(1)))");
}

/// INVARIANT: `int64ToWord64# :: Int64# -> Word64#` tags its result Word.
/// Both engines compute the same bit pattern for `i64::MIN`; this pins that
/// the JIT's result carries `LIT_TAG_WORD`, not `LIT_TAG_INT`.
#[test]
fn jitbug_int64_to_word64_result_tag() {
    let (eval, jit) = run_one(PrimOpKind::Int64ToWord64, vec![Literal::LitInt(i64::MIN)]);
    assert_eq!(eval, "Ok(Lit(LitWord(9223372036854775808)))");
    assert_eq!(jit, "Ok(Lit(LitWord(9223372036854775808)))");
}

// ===========================================================================
// Configs.
//
// The int/word lanes loop over MANY ops per case, so 350 cases is thousands
// of compiled programs; the float lanes are lighter per case, so cases are
// raised to 400.
// ===========================================================================
fn cfg_int() -> Config {
    let mut c = Config::with_cases(350);
    c.max_shrink_iters = 5000;
    // The `proptest!` macro sets this implicitly from `file!()`; a hand-built
    // Config driven through TestRunner does not, and FileFailurePersistence::
    // SourceParallel (the default) silently resolves to NO PATH without it —
    // counterexamples would stop persisting/replaying with no test failure to
    // report it. Must be set at THIS call site, not a shared helper, so
    // `file!()` expands to this lane's own path.
    c.source_file = Some(file!());
    c
}

fn cfg_float() -> Config {
    let mut c = Config::with_cases(400);
    c.max_shrink_iters = 5000;
    c.source_file = Some(file!());
    c
}

/// Independent IEEE arithmetic oracle: exact bits except NaN payload/sign,
/// which arithmetic does not promise to preserve. Signed zero is significant.
/// Host Rust is an independent lowering oracle, NOT the native-GHC oracle.
fn assert_ieee_result(op: PrimOpKind, args: Vec<Literal>, expected: Literal) {
    let mut builder = TreeBuilder::new();
    let inputs = args
        .iter()
        .cloned()
        .map(|x| builder.push(CoreFrame::Lit(x)))
        .collect();
    builder.push(CoreFrame::PrimOp { op, args: inputs });
    let tree = builder.build();
    let table = build_table_for_expr(&tree);
    let mut heap = VecHeap::new();
    let reference =
        eval(&tree, &env_from_datacon_table(&table), &mut heap).expect("reference evaluation");
    let compiled = JitEffectMachine::compile(&tree, &table, 64 * 1024)
        .and_then(|mut machine| machine.run_pure())
        .expect("JIT evaluation");
    for (engine, value) in [("reference", reference), ("JIT", compiled)] {
        let Value::Lit(ref actual) = value else {
            panic!("{engine}: nonliteral {value:?}")
        };
        let equal = match (actual, &expected) {
            (Literal::LitFloat(a), Literal::LitFloat(b)) => {
                a == b || (f32::from_bits(*a as u32).is_nan() && f32::from_bits(*b as u32).is_nan())
            }
            (Literal::LitDouble(a), Literal::LitDouble(b)) => {
                a == b || (f64::from_bits(*a).is_nan() && f64::from_bits(*b).is_nan())
            }
            _ => *actual == expected,
        };
        assert!(
            equal,
            "{engine} {op:?} {args:?}: {actual:?} != {expected:?}"
        );
    }
}

#[test]
#[serial]
fn ieee_edge_bits_independent_oracle() {
    macro_rules! width {
        ($ty:ty, $lit:ident, $add:ident, $sub:ident, $mul:ident, $div:ident,
         $eq:ident, $lt:ident, $neg:ident, $convert:ident, $out:ident, $other:ty) => {{
            let values: [$ty; 12] = [
                0.0,
                -0.0,
                1.0,
                -1.0,
                <$ty>::from_bits(1),
                -<$ty>::from_bits(1),
                <$ty>::MIN_POSITIVE,
                <$ty>::MAX,
                -<$ty>::MAX,
                <$ty>::INFINITY,
                <$ty>::NEG_INFINITY,
                <$ty>::NAN,
            ];
            for a in values {
                let input = Literal::$lit(a.to_bits() as u64);
                assert_ieee_result(
                    PrimOpKind::$neg,
                    vec![input.clone()],
                    Literal::$lit((-a).to_bits() as u64),
                );
                assert_ieee_result(
                    PrimOpKind::$convert,
                    vec![input.clone()],
                    Literal::$out((a as $other).to_bits() as u64),
                );
                // Rotate across boundary operands without a costly full Cartesian battery.
                for b in [0.0, -1.0, <$ty>::from_bits(1), <$ty>::INFINITY] {
                    let args = vec![input.clone(), Literal::$lit(b.to_bits() as u64)];
                    for (op, answer) in [
                        (PrimOpKind::$add, a + b),
                        (PrimOpKind::$sub, a - b),
                        (PrimOpKind::$mul, a * b),
                        (PrimOpKind::$div, a / b),
                    ] {
                        assert_ieee_result(
                            op,
                            args.clone(),
                            Literal::$lit(answer.to_bits() as u64),
                        );
                    }
                    assert_ieee_result(
                        PrimOpKind::$eq,
                        args.clone(),
                        Literal::LitInt((a == b) as i64),
                    );
                    assert_ieee_result(PrimOpKind::$lt, args, Literal::LitInt((a < b) as i64));
                }
            }
        }};
    }
    width!(
        f32,
        LitFloat,
        FloatAdd,
        FloatSub,
        FloatMul,
        FloatDiv,
        FloatEq,
        FloatLt,
        FloatNegate,
        Float2Double,
        LitDouble,
        f64
    );
    width!(
        f64,
        LitDouble,
        DoubleAdd,
        DoubleSub,
        DoubleMul,
        DoubleDiv,
        DoubleEq,
        DoubleLt,
        DoubleNegate,
        Double2Float,
        LitFloat,
        f32
    );
}
