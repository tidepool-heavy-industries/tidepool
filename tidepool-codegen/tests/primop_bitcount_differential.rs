//! Differential coverage for the bit-count primops (`popCnt#`/`popCntN#`,
//! `ctz#`/`ctzN#`).
//!
//! These were missing from the tree-walking interpreter's primop dispatch
//! (`UnsupportedPrimOp`), so on the corpus's Double-literal / Rational→Double
//! paths eval errored while the JIT ran — `check_jit_vs_eval`'s both-fail /
//! eval-fail arms then MASKED whatever the JIT did. Now that eval implements
//! them to spec, this pins eval == JIT == the defined semantics.

use tidepool_repr::types::{Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_testing::proptest::{build_table_for_expr, check_jit_vs_eval};

/// `op applied to (Word# n)` as a standalone program.
fn build_unary_word(op: PrimOpKind, n: u64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let arg = b.push(CoreFrame::Lit(Literal::LitWord(n)));
    let _root = b.push(CoreFrame::PrimOp {
        op,
        args: vec![arg],
    });
    b.build()
}

fn eval_word(expr: &CoreExpr) -> u64 {
    let table = build_table_for_expr(expr);
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    match eval(expr, &env, &mut heap).expect("eval") {
        Value::Lit(Literal::LitWord(w)) => w,
        Value::Lit(Literal::LitInt(i)) => i as u64,
        other => panic!("expected a word result, got {other:?}"),
    }
}

fn jit_word(expr: &CoreExpr) -> u64 {
    let table = build_table_for_expr(expr);
    let mut m = JitEffectMachine::compile(expr, &table, 64 * 1024).expect("JIT compile");
    match m.run_pure().expect("JIT run") {
        Value::Lit(Literal::LitWord(w)) => w,
        Value::Lit(Literal::LitInt(i)) => i as u64,
        other => panic!("expected a word result from JIT, got {other:?}"),
    }
}

/// Reference semantics (GHC spec): popCntN# = set-bit count of the low N bits;
/// ctzN# = trailing zeros of the low N bits, = N when those bits are all zero.
fn spec(op: PrimOpKind, n: u64) -> u64 {
    let r = match op {
        PrimOpKind::PopCnt | PrimOpKind::PopCnt64 => n.count_ones(),
        PrimOpKind::PopCnt8 => (n as u8).count_ones(),
        PrimOpKind::PopCnt16 => (n as u16).count_ones(),
        PrimOpKind::PopCnt32 => (n as u32).count_ones(),
        PrimOpKind::Ctz | PrimOpKind::Ctz64 => n.trailing_zeros(),
        PrimOpKind::Ctz8 => (n as u8).trailing_zeros(),
        PrimOpKind::Ctz16 => (n as u16).trailing_zeros(),
        PrimOpKind::Ctz32 => (n as u32).trailing_zeros(),
        _ => unreachable!(),
    };
    r as u64
}

const OPS: &[PrimOpKind] = &[
    PrimOpKind::PopCnt,
    PrimOpKind::PopCnt8,
    PrimOpKind::PopCnt16,
    PrimOpKind::PopCnt32,
    PrimOpKind::PopCnt64,
    PrimOpKind::Ctz,
    PrimOpKind::Ctz8,
    PrimOpKind::Ctz16,
    PrimOpKind::Ctz32,
    PrimOpKind::Ctz64,
];

// A spread of inputs: zero, single bits at every byte boundary, alternating
// patterns, sub-width values, and all-ones.
const INPUTS: &[u64] = &[
    0,
    1,
    0b1011,
    0x80,
    0x100,
    0xFF,
    0xFFFF,
    0xFF_FFFF,
    0xFFFF_FFFF,
    0x1_0000_0000,
    0xDEAD_BEEF,
    0xFFFF_FFFF_FFFF_FFFF,
    0xA5A5_A5A5_A5A5_A5A5,
];

#[test]
fn bitcount_eval_matches_spec() {
    for &op in OPS {
        for &n in INPUTS {
            let expr = build_unary_word(op, n);
            assert_eq!(eval_word(&expr), spec(op, n), "eval {op} {n:#x}");
        }
    }
}

#[test]
fn bitcount_eval_equals_jit() {
    for &op in OPS {
        for &n in INPUTS {
            let expr = build_unary_word(op, n);
            let ev = eval_word(&expr);
            let jit = jit_word(&expr);
            assert_eq!(ev, spec(op, n), "eval-vs-spec {op} {n:#x}");
            assert_eq!(jit, spec(op, n), "jit-vs-spec {op} {n:#x}");
            assert_eq!(ev, jit, "eval/jit divergence {op} {n:#x}");
        }
    }
}

#[test]
fn bitcount_through_differential_oracle() {
    for &op in OPS {
        for &n in INPUTS {
            let expr = build_unary_word(op, n);
            check_jit_vs_eval(expr, 64 * 1024)
                .unwrap_or_else(|e| panic!("differential oracle failed for {op} {n:#x}: {e:?}"));
        }
    }
}
