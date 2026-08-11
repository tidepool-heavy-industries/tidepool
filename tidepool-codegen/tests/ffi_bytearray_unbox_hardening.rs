//! Hardening regression for the codex-review-2026-08-08.md item 1 follow-up:
//! `unbox_bytearray` (`tidepool-codegen/src/emit/primop.rs`) had its own
//! duplicated constructor-unwrap loop — a copy of the one `unbox_addr`
//! carried before its own hardening (`ffi_strlen_unbox_hardening.rs`) — with
//! NO final `TAG_LIT` check before treating a Con's unwrapped payload as a
//! byte-array/boxed-array pointer and dereferencing it. Same tag-as-address
//! escape class, same fix pattern: reject `Raw` SSA values that aren't
//! statically `LIT_TAG_BYTEARRAY`, and require the final payload (after any
//! 1-field wrapper unwrap) be `TAG_LIT` with an array-carrying class
//! (`String#`/`ByteArray#`/`SmallArray#`/`Array#`) before its payload is read
//! as a pointer.
//!
//! `unbox_bytearray` is shared by every `ByteArray#`/`SmallArray#`/`Array#`-
//! consuming primop (`SizeofByteArray`, `IndexWord8Array`,
//! `WriteWord8Array`, `CopyByteArray`, `ByteArrayContents`, `ReadArray`,
//! `WriteSmallArray`, and more — see the call sites in `primop.rs`), so
//! hardening it covers all of them; `SizeofByteArray` is exercised directly
//! here since it dereferences the unboxed pointer immediately (a single
//! `load` at offset 0, no host-fn indirection), making it the sharpest
//! reproducer.
//!
//! Builds a minimal `CoreExpr` by hand, compiles, and asserts `run_pure()`
//! surfaces a clean typed `RuntimeError` (`Err(Yield(Runtime(_)))`) rather
//! than a caught signal (`Err(Yield(Signal(_)))`, meaning the process
//! actually SIGSEGV'd) or a silent wrong answer.

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{DataConId, Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

/// A nullary constructor — no fields at all, so a heap pointer to it is
/// nothing like a `ByteArray#`/`SmallArray#`/`Array#` literal.
const NULLARY: DataConId = DataConId(9101);
/// A 1-field constructor wrapping a plain `Int#`, structurally identical to a
/// legitimate boxing wrapper but carrying a payload of the WRONG literal
/// class — exactly the shape the review flagged for `unbox_addr` and, by the
/// same duplicated loop, for `unbox_bytearray` too.
const INT_WRAPPER: DataConId = DataConId(9102);

fn table() -> DataConTable {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: NULLARY,
        name: "Nullary".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: INT_WRAPPER,
        name: "IntWrapper".to_string(),
        tag: 2,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t
}

/// Assert `run_pure()` failed CLEANLY (a typed `RuntimeError`, not a signal
/// the safety net had to catch, and not `Ok`).
fn assert_clean_failure(result: Result<Value, JitError>, what: &str) {
    match result {
        Err(JitError::Yield(YieldError::Signal(sig))) => panic!(
            "[{what}] unbox_bytearray dereferenced a non-array value and the process \
             SIGSEGV'd (signal {sig}); with_signal_protection caught it, but the \
             deref itself is the codex-review-2026-08-08 item-1-follow-up regression"
        ),
        Err(JitError::Yield(YieldError::Runtime(_))) => {} // expected: clean typed error
        other => panic!("[{what}] expected Err(Yield(Runtime(_))), got {other:?}"),
    }
}

/// `sizeofByteArray# (unsafeCoerce# Nullary)` — a nullary constructor fed
/// straight to `SizeofByteArray`. `unbox_bytearray`'s con-unwrap loop finds
/// `TAG_CON` with 0 fields, which fails the existing boxing-wrapper-arity
/// guard (`BoxingArity`, shared via `emit_boxing_wrapper_guard`) before ever
/// reaching the array-class check this task adds — confirming the
/// pre-existing arity guard still covers this shape after the bytearray
/// hardening.
#[test]
fn nullary_con_into_sizeof_bytearray_fails_cleanly() {
    let mut b = TreeBuilder::new();
    let con = b.push(CoreFrame::Con {
        tag: NULLARY,
        fields: vec![],
    });
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::SizeofByteArray,
        args: vec![con],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &table(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    assert_clean_failure(machine.run_pure(), "nullary_con");
}

/// `sizeofByteArray# (unsafeCoerce# (IntWrapper 0x5000_0000#))` — a 1-field
/// constructor whose payload is a real heap `Lit` (so it clears the arity
/// guard cleanly) but of the WRONG literal class (`Int#`, not
/// `String#`/`ByteArray#`/`SmallArray#`/`Array#`). The payload value is a
/// plausible-looking but unmapped address, well above any valid heap
/// address, chosen so that — pre-fix — `unbox_bytearray` loads it as `ba_ptr`
/// and `SizeofByteArray` immediately dereferences it (`load ba_ptr, 0`) with
/// no host-fn guard in between, unlike the strlen path. This is the exact
/// escape the review named for `unbox_addr`, reproduced against
/// `unbox_bytearray`'s own copy of the same unguarded con-unwrap loop.
#[test]
fn one_field_non_array_con_into_sizeof_bytearray_fails_cleanly() {
    let mut b = TreeBuilder::new();
    let bogus_addr = b.push(CoreFrame::Lit(Literal::LitInt(0x5000_0000)));
    let wrapped = b.push(CoreFrame::Con {
        tag: INT_WRAPPER,
        fields: vec![bogus_addr],
    });
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::SizeofByteArray,
        args: vec![wrapped],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &table(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    assert_clean_failure(machine.run_pure(), "one_field_int_wrapper");
}

/// Positive control: a real `ByteArray#` literal (`LitByteArray`, the actual
/// shape `SizeofByteArray` is meant to consume) still flows through
/// unchanged and `sizeofByteArray#` returns its length. Proves the hardening
/// didn't regress the legitimate path.
#[test]
fn real_bytearray_literal_into_sizeof_bytearray_still_works() {
    let mut b = TreeBuilder::new();
    let ba = b.push(CoreFrame::Lit(Literal::LitByteArray(vec![1, 2, 3, 4, 5])));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::SizeofByteArray,
        args: vec![ba],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &table(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    let result = machine
        .run_pure()
        .unwrap_or_else(|e| panic!("expected Ok, got {e:?}"));
    match result {
        Value::Lit(Literal::LitInt(n)) => {
            assert_eq!(n, 5, "sizeofByteArray#([1,2,3,4,5]) should be 5")
        }
        other => panic!("expected Value::Lit(LitInt(5)), got {other:?}"),
    }
}
