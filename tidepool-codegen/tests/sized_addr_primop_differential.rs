//! Differential coverage (JIT == tree-walker == GHC spec) for the
//! engine-hardening sized-int/word and Addr# primops:
//!   - Word8: gtWord8# / quotWord8# / remWord8# / timesWord8#
//!   - Int8 conversions: int8ToInt# / int8ToWord8# / word8ToInt8# / negateInt8#
//!   - Word32/Int32: int32ToInt# / word32ToWord# / wordToWord32# / gtWord32# /
//!     leWord32# / ltWord32# / plusWord32# / subWord32#
//!   - Addr#: eqAddr# / minusAddr# / indexInt8OffAddr# / indexWord32OffAddr#
//!
//! Addr# tests bind the address to a `let` and reuse the SAME binding so the
//! JIT (pointer identity) and eval (byte-content) agree on equality/difference.

use tidepool_repr::types::{Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder, VarId};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_testing::proptest::build_table_for_expr;

fn build_unary(op: PrimOpKind, arg: Literal) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let a = b.push(CoreFrame::Lit(arg));
    let _root = b.push(CoreFrame::PrimOp { op, args: vec![a] });
    b.build()
}

fn build_binary(op: PrimOpKind, x: Literal, y: Literal) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let a = b.push(CoreFrame::Lit(x));
    let c = b.push(CoreFrame::Lit(y));
    let _root = b.push(CoreFrame::PrimOp {
        op,
        args: vec![a, c],
    });
    b.build()
}

/// `let v = <addr> in op v v` (binary addr op on the same binding).
fn build_addr_self_binary(op: PrimOpKind, addr: Vec<u8>) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let v = VarId(1);
    let rhs = b.push(CoreFrame::Lit(Literal::LitString(addr)));
    let v1 = b.push(CoreFrame::Var(v));
    let v2 = b.push(CoreFrame::Var(v));
    let prim = b.push(CoreFrame::PrimOp {
        op,
        args: vec![v1, v2],
    });
    let _root = b.push(CoreFrame::LetNonRec {
        binder: v,
        rhs,
        body: prim,
    });
    b.build()
}

/// `let v = <addr> in op v <idx>` (off-addr read on a single addr binding).
fn build_addr_index(op: PrimOpKind, addr: Vec<u8>, idx: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let v = VarId(1);
    let rhs = b.push(CoreFrame::Lit(Literal::LitString(addr)));
    let vref = b.push(CoreFrame::Var(v));
    let i = b.push(CoreFrame::Lit(Literal::LitInt(idx)));
    let prim = b.push(CoreFrame::PrimOp {
        op,
        args: vec![vref, i],
    });
    let _root = b.push(CoreFrame::LetNonRec {
        binder: v,
        rhs,
        body: prim,
    });
    b.build()
}

fn eval_scalar(expr: &CoreExpr) -> i64 {
    let table = build_table_for_expr(expr);
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    match eval(expr, &env, &mut heap).expect("eval") {
        Value::Lit(Literal::LitWord(w)) => w as i64,
        Value::Lit(Literal::LitInt(i)) => i,
        Value::Lit(Literal::LitChar(c)) => c as i64,
        other => panic!("expected a scalar result, got {other:?}"),
    }
}

fn jit_scalar(expr: &CoreExpr) -> i64 {
    let table = build_table_for_expr(expr);
    let mut m = JitEffectMachine::compile(expr, &table, 64 * 1024).expect("JIT compile");
    match m.run_pure().expect("JIT run") {
        Value::Lit(Literal::LitWord(w)) => w as i64,
        Value::Lit(Literal::LitInt(i)) => i,
        Value::Lit(Literal::LitChar(c)) => c as i64,
        other => panic!("expected a scalar result from JIT, got {other:?}"),
    }
}

fn assert_all_agree(expr: &CoreExpr, spec: i64, label: &str) {
    let ev = eval_scalar(expr);
    let jit = jit_scalar(expr);
    assert_eq!(ev, spec, "eval-vs-spec {label}");
    assert_eq!(jit, spec, "jit-vs-spec {label}");
    assert_eq!(ev, jit, "eval/jit divergence {label}");
}

#[test]
fn word8_ops_agree() {
    let pairs: &[(u64, u64)] = &[(0, 1), (200, 7), (255, 16), (100, 3), (13, 5), (250, 250)];
    for &(a, b) in pairs {
        let w = |x| Literal::LitWord(x);
        assert_all_agree(
            &build_binary(PrimOpKind::Word8Gt, w(a), w(b)),
            i64::from(a > b),
            &format!("gtWord8# {a} {b}"),
        );
        assert_all_agree(
            &build_binary(PrimOpKind::Word8Mul, w(a), w(b)),
            ((a.wrapping_mul(b)) & 0xFF) as i64,
            &format!("timesWord8# {a} {b}"),
        );
        if b != 0 {
            assert_all_agree(
                &build_binary(PrimOpKind::Word8Quot, w(a), w(b)),
                ((a & 0xFF) / (b & 0xFF)) as i64,
                &format!("quotWord8# {a} {b}"),
            );
            assert_all_agree(
                &build_binary(PrimOpKind::Word8Rem, w(a), w(b)),
                ((a & 0xFF) % (b & 0xFF)) as i64,
                &format!("remWord8# {a} {b}"),
            );
        }
    }
}

#[test]
fn int8_conversions_agree() {
    for &n in &[0i64, 1, 127, 128, 200, 255, -1, -128] {
        assert_all_agree(
            &build_unary(PrimOpKind::Int8ToInt, Literal::LitInt(n)),
            (n as i8) as i64,
            &format!("int8ToInt# {n}"),
        );
        assert_all_agree(
            &build_unary(PrimOpKind::Word8ToInt8, Literal::LitWord((n as u8) as u64)),
            (n as u8 as i8) as i64,
            &format!("word8ToInt8# {n}"),
        );
        assert_all_agree(
            &build_unary(PrimOpKind::Int8ToWord8, Literal::LitInt(n)),
            (n as u8) as i64,
            &format!("int8ToWord8# {n}"),
        );
        assert_all_agree(
            &build_unary(PrimOpKind::Int8Negate, Literal::LitInt(n)),
            ((n as i8).wrapping_neg()) as i64,
            &format!("negateInt8# {n}"),
        );
    }
}

#[test]
fn word32_int32_ops_agree() {
    let w = Literal::LitWord;
    for &n in &[0u64, 1, 0xFFFF, 0x8000_0000, 0xFFFF_FFFF, 0x1_0000_0000] {
        assert_all_agree(
            &build_unary(PrimOpKind::Word32ToWord, w(n)),
            (n & 0xFFFF_FFFF) as i64,
            &format!("word32ToWord# {n}"),
        );
        assert_all_agree(
            &build_unary(PrimOpKind::WordToWord32, w(n)),
            (n & 0xFFFF_FFFF) as i64,
            &format!("wordToWord32# {n}"),
        );
        assert_all_agree(
            &build_unary(PrimOpKind::Int32ToInt, Literal::LitInt(n as i64)),
            (n as i32) as i64,
            &format!("int32ToInt# {n}"),
        );
    }
    let pairs: &[(u64, u64)] = &[
        (0, 1),
        (0xFFFF_FFFF, 1),
        (5, 5),
        (0x8000_0000, 0x7FFF_FFFF),
        (100, 200),
    ];
    for &(a, b) in pairs {
        assert_all_agree(
            &build_binary(PrimOpKind::Word32Add, w(a), w(b)),
            ((a.wrapping_add(b)) & 0xFFFF_FFFF) as i64,
            &format!("plusWord32# {a} {b}"),
        );
        assert_all_agree(
            &build_binary(PrimOpKind::Word32Sub, w(a), w(b)),
            ((a.wrapping_sub(b)) & 0xFFFF_FFFF) as i64,
            &format!("subWord32# {a} {b}"),
        );
        assert_all_agree(
            &build_binary(PrimOpKind::Word32Gt, w(a), w(b)),
            i64::from(a > b),
            &format!("gtWord32# {a} {b}"),
        );
        assert_all_agree(
            &build_binary(PrimOpKind::Word32Le, w(a), w(b)),
            i64::from(a <= b),
            &format!("leWord32# {a} {b}"),
        );
        assert_all_agree(
            &build_binary(PrimOpKind::Word32Lt, w(a), w(b)),
            i64::from(a < b),
            &format!("ltWord32# {a} {b}"),
        );
    }
}

#[test]
fn addr_ops_agree() {
    // eqAddr# of an addr with itself is always true (1); minusAddr# of an addr
    // with itself is always 0. Both hold for pointer-identity (JIT) and
    // byte-content (eval) semantics.
    let addr = b"hello, addr".to_vec();
    assert_all_agree(
        &build_addr_self_binary(PrimOpKind::EqAddr, addr.clone()),
        1,
        "eqAddr# self",
    );
    assert_all_agree(
        &build_addr_self_binary(PrimOpKind::MinusAddr, addr.clone()),
        0,
        "minusAddr# self",
    );

    // indexInt8OffAddr# reads a signed byte at index i.
    let bytes = vec![0x00u8, 0x7F, 0x80, 0xFF, 0x01];
    for (i, &raw) in bytes.iter().enumerate() {
        assert_all_agree(
            &build_addr_index(PrimOpKind::IndexInt8OffAddr, bytes.clone(), i as i64),
            (raw as i8) as i64,
            &format!("indexInt8OffAddr# [{i}]"),
        );
    }

    // indexWord32OffAddr# reads a 32-bit word at a 4-byte stride.
    let mut buf = Vec::new();
    buf.extend_from_slice(&0x1234_5678u32.to_ne_bytes());
    buf.extend_from_slice(&0xFFFF_0000u32.to_ne_bytes());
    for (i, &expected) in [0x1234_5678u32, 0xFFFF_0000u32].iter().enumerate() {
        assert_all_agree(
            &build_addr_index(PrimOpKind::IndexWord32OffAddr, buf.clone(), i as i64),
            expected as i64,
            &format!("indexWord32OffAddr# [{i}]"),
        );
    }
}
