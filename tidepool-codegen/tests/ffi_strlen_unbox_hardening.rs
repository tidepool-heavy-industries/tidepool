//! Hardening regression for codex-review-2026-08-08.md item 1: the FfiStrlen
//! unbox path (`unbox_addr`, `tidepool-codegen/src/emit/primop.rs`) accepted
//! any `SsaVal::Raw` without checking its static literal tag, and — for
//! `SsaVal::HeapPtr` values — recursively unwrapped any 1-field constructor
//! and loaded its payload as an address WITHOUT first requiring the payload
//! be a `TAG_LIT` of an address-carrying class. A stray tag/int word wrapped
//! in a 1-field Con (or reaching the primop as a bare nullary Con) could
//! reach `runtime_strlen` as a raw pointer.
//!
//! `unbox_addr` is shared by every `Addr#`-consuming primop (`PlusAddr`,
//! `IndexWord8OffAddr`, `EqAddr`, `MinusAddr`, `IndexAddrOffAddr`,
//! `IndexCharOffAddr`, `WriteWord8OffAddr`, `CopyAddrToByteArray`, and more —
//! see the call sites in `primop.rs`), so hardening it covers all of them;
//! `FfiStrlen` is exercised directly here as the primop this finding named.
//!
//! Builds a minimal `CoreExpr` by hand, compiles, and asserts `run_pure()`
//! surfaces a clean typed `RuntimeError` (`Err(Yield(Runtime(_)))`) rather
//! than a caught signal (`Err(Yield(Signal(_)))`, meaning the process
//! actually SIGSEGV'd and `with_signal_protection` only kept the test binary
//! alive) or a silent wrong answer.

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{DataConId, Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

/// A nullary constructor — no fields at all, so a heap pointer to it is
/// nothing like an `Addr#`/`String#`/`ByteArray#` literal.
const NULLARY: DataConId = DataConId(9001);
/// A 1-field constructor wrapping a plain `Int#`, structurally identical to a
/// legitimate boxing wrapper (`I#`, or `Ptr#` around an `Addr#`) but carrying
/// a payload of the WRONG literal class — exactly the shape the review
/// flagged: "recursively unwraps any one-field constructor and reads its
/// literal payload as an address WITHOUT requiring a literal tag."
const INT_WRAPPER: DataConId = DataConId(9002);

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
            "[{what}] unbox_addr dereferenced a non-address value and the process \
             SIGSEGV'd (signal {sig}); with_signal_protection caught it, but the \
             deref itself is the codex-review-2026-08-08 item-1 regression"
        ),
        Err(JitError::Yield(YieldError::Runtime(_))) => {} // expected: clean typed error
        other => panic!("[{what}] expected Err(Yield(Runtime(_))), got {other:?}"),
    }
}

/// `strlen# (unsafeCoerce# Nullary)` — a nullary constructor fed straight to
/// `FfiStrlen`. `unbox_addr`'s con-unwrap loop finds `TAG_CON` with 0 fields,
/// which fails the existing boxing-wrapper-arity guard (`BoxingArity`) before
/// ever reaching the address-class check this task adds — confirming the
/// pre-existing arity guard still covers this shape after the addr hardening.
#[test]
fn nullary_con_into_ffi_strlen_fails_cleanly() {
    let mut b = TreeBuilder::new();
    let con = b.push(CoreFrame::Con {
        tag: NULLARY,
        fields: vec![],
    });
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::FfiStrlen,
        args: vec![con],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &table(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    assert_clean_failure(machine.run_pure(), "nullary_con");
}

/// `strlen# (unsafeCoerce# (IntWrapper 0x5000_0000#))` — a 1-field
/// constructor whose payload is a real heap `Lit` (so it clears the arity
/// guard cleanly) but of the WRONG literal class (`Int#`, not
/// `Addr#`/`String#`/`ByteArray#`). The payload value is a plausible-looking
/// but unmapped address, well above `MIN_VALID_ADDR` — chosen so that,
/// pre-fix, `unbox_addr` would load it and hand it straight to
/// `runtime_strlen` as a real dereference target instead of being caught by
/// that host fn's own low-address guard. This is the exact escape the review
/// named: a Con's single field loaded and used as an address with no
/// literal-tag check.
#[test]
fn one_field_non_address_con_into_ffi_strlen_fails_cleanly() {
    let mut b = TreeBuilder::new();
    let bogus_addr = b.push(CoreFrame::Lit(Literal::LitInt(0x5000_0000)));
    let wrapped = b.push(CoreFrame::Con {
        tag: INT_WRAPPER,
        fields: vec![bogus_addr],
    });
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::FfiStrlen,
        args: vec![wrapped],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &table(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    assert_clean_failure(machine.run_pure(), "one_field_int_wrapper");
}

/// Positive control: a real string literal (`LitString`, the actual
/// `Addr#`/`String#` shape `FfiStrlen` is meant to consume) still flows
/// through unchanged and `strlen#` returns its length. Proves the hardening
/// didn't regress the legitimate path.
#[test]
fn real_string_literal_into_ffi_strlen_still_works() {
    let mut b = TreeBuilder::new();
    let s = b.push(CoreFrame::Lit(Literal::LitString(b"hello".to_vec())));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::FfiStrlen,
        args: vec![s],
    });
    let expr: CoreExpr = b.build();

    let mut machine = JitEffectMachine::compile(&expr, &table(), 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    let result = machine
        .run_pure()
        .unwrap_or_else(|e| panic!("expected Ok, got {e:?}"));
    match result {
        Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 5, "strlen(\"hello\") should be 5"),
        other => panic!("expected Value::Lit(LitInt(5)), got {other:?}"),
    }
}
