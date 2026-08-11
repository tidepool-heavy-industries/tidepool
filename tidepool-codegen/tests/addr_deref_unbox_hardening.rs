//! Hardening regression, found by code audit (NOT a repro of any specific
//! reported crash — see this file's own tests for what it does and does not
//! cover): `unbox_addr`'s `SsaVal::Raw(v, tag)` branch
//! (`tidepool-codegen/src/emit/primop.rs`) trusts its *static* literal tag
//! unconditionally — a compile-time label the emitter attaches when it
//! believes it knows the value's provenance, not a runtime check on the
//! VALUE. That is fine for `unbox_addr` itself: a legitimate `Addr#`
//! computation (`plusAddr#`, `eqAddr#`, `minusAddr#`) must be free to hold,
//! and compute with, a null or out-of-range address without tripping a
//! trap — only an actual DEREFERENCE may reject one.
//!
//! But nothing validated the resulting VALUE anywhere on the path to a
//! dereference for the class of `Addr#`-consuming primop that never calls a
//! host fn (`IndexCharOffAddr`, `IndexWord8OffAddr`, `WriteWord8OffAddr`,
//! `IndexAddrOffAddr`, `IndexInt8OffAddr`, `IndexWord32OffAddr`,
//! `IndexWideCharOffAddr`, `WriteWideCharOffAddr`): each emitted a raw
//! Cranelift `load`/`store` directly against the address with
//! `MemFlags::trusted()`, with nothing standing between a bad pointer and
//! the memory access. `FfiStrlen` and its FFI siblings are NOT in this
//! list — they call a host fn (`runtime_strlen`, ...) that self-checks via
//! `check_ptr_invalid`.
//!
//! `IndexAddrArray` (`indexAddrArray# :: ByteArray# -> Int# -> Addr#`) is
//! the easiest way to produce a legitimately-typed-but-bad address: it loads
//! whatever 8 bytes sit in a `ByteArray#` slot and returns them as
//! `Raw(_, LIT_TAG_ADDR)` verbatim — a zero-filled slot (a perfectly legal
//! `ByteArray#` payload; nothing requires a slot meant to hold an address to
//! already contain one) round-trips as address 0 with no check anywhere.
//! Feeding that into `IndexWord8OffAddr` dereferences it directly.
//!
//! These tests build a minimal `CoreExpr` by hand, compile, and assert
//! `run_pure()` surfaces a clean typed `RuntimeError`
//! (`Err(Yield(Runtime(_)))`) rather than a caught signal
//! (`Err(Yield(Signal(_)))`, meaning the process actually SIGSEGV'd and
//! `with_signal_protection` only kept the test binary alive) or a silent
//! wrong answer.

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_repr::types::{Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

/// Assert `run_pure()` failed CLEANLY (a typed `RuntimeError`, not a signal
/// the safety net had to catch, and not `Ok`).
fn assert_clean_failure(result: Result<Value, JitError>, what: &str) {
    match result {
        Err(JitError::Yield(YieldError::Signal(sig))) => panic!(
            "[{what}] a null address reached an in-JIT load/store and the process \
             SIGSEGV'd (signal {sig}); with_signal_protection caught it, but the \
             deref itself is the addr-deref-guard regression"
        ),
        Err(JitError::Yield(YieldError::Runtime(_))) => {} // expected: clean typed error
        other => panic!("[{what}] expected Err(Yield(Runtime(_))), got {other:?}"),
    }
}

/// `indexWord8OffAddr# (indexAddrArray# zero_filled_bytearray 0#) 0#` — an
/// `Addr#` read out of a zero-filled `ByteArray#` slot (a legitimate,
/// well-typed way to obtain a null-but-`LIT_TAG_ADDR`-tagged `Raw` value),
/// fed straight into a primop that dereferences it with a raw Cranelift
/// `load`. Before the `emit_addr_deref_guard` hardening, nothing checked the
/// address between `IndexAddrArray`'s load and `IndexWord8OffAddr`'s own
/// load — the exact escape this task closes.
#[test]
fn null_addr_from_bytearray_into_index_word8_off_addr_fails_cleanly() {
    let mut b = TreeBuilder::new();
    let ba = b.push(CoreFrame::Lit(Literal::LitByteArray(vec![0u8; 8])));
    let idx0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let addr = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexAddrArray,
        args: vec![ba, idx0],
    });
    let off0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexWord8OffAddr,
        args: vec![addr, off0],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &DataConTable::new(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    assert_clean_failure(machine.run_pure(), "null_addr_index_word8_off_addr");
}

/// Same null-address source, fed to `writeWord8OffAddr#` — the store-side
/// sibling of the read above, and the other shape `emit_addr_deref_guard`
/// covers (`MemFlags::trusted()` `store`, not `load`).
#[test]
fn null_addr_from_bytearray_into_write_word8_off_addr_fails_cleanly() {
    let mut b = TreeBuilder::new();
    let ba = b.push(CoreFrame::Lit(Literal::LitByteArray(vec![0u8; 8])));
    let idx0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let addr = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexAddrArray,
        args: vec![ba, idx0],
    });
    let off0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let val = b.push(CoreFrame::Lit(Literal::LitInt(0x42)));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::WriteWord8OffAddr,
        args: vec![addr, off0, val],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &DataConTable::new(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    assert_clean_failure(machine.run_pure(), "null_addr_write_word8_off_addr");
}

/// Positive control: a REAL, valid address (from a real string literal,
/// bytes well within the allocation) still round-trips through
/// `IndexWord8OffAddr` unchanged — the hardening does not disturb legitimate
/// address arithmetic or dereference.
#[test]
fn real_address_into_index_word8_off_addr_still_works() {
    let mut b = TreeBuilder::new();
    let s = b.push(CoreFrame::Lit(Literal::LitString(b"hello".to_vec())));
    let off0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexCharOffAddr,
        args: vec![s, off0],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &DataConTable::new(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    let result = machine
        .run_pure()
        .unwrap_or_else(|e| panic!("expected Ok, got {e:?}"));
    match result {
        Value::Lit(Literal::LitChar(c)) => {
            assert_eq!(c, 'h', "indexCharOffAddr# 0 on \"hello\" should be 'h'")
        }
        other => panic!("expected Value::Lit(LitChar('h')), got {other:?}"),
    }
}
