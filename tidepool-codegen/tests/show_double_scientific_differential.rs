//! Differential fixture for the numeric decode/show policy drift (duplication
//! survey finding 6): `tidepool-eval`'s `ShowDoubleAddr`/`ShowSignedDoubleAddr`
//! primop handling and `tidepool-codegen`'s `runtime_show_double_addr` host fn
//! both used to carry their own copy of Haskell's `Show Double` scientific-
//! notation formatting. The JIT's copy had the BUG-1 fix (inserting a decimal
//! point into an integral scientific-notation mantissa, e.g. `"1.0e10"`
//! instead of Rust's bare `"1e10"`, pinned by
//! `proptest_host_arrays::bug1_show_double_scientific_decimal`); the oracle's
//! copy predated that fix and would show `"1e10"`.
//!
//! Both backends now call the ONE shared implementation
//! (`tidepool_bignum::haskell_show_double`), which closes the drift by
//! construction rather than by keeping two copies in sync. This fixture
//! exercises each backend's REAL production call site — the oracle's
//! `eval_at` `PrimOpKind::ShowDoubleAddr` arm, and the JIT's
//! `runtime_show_double_addr` host fn (`emit/primop.rs` compiles
//! `ShowDoubleAddr` as a direct call into this same host fn; there is no
//! separate Cranelift-IR reimplementation of the formatting logic) — and
//! confirms they now agree over a set of doubles that land in scientific
//! notation with an integral mantissa, the exact shape BUG-1 covers.
//!
//! Divergence verdict: this drift IS real and reproducible (this fixture goes
//! red on the pre-fix oracle if `eval_at`'s `ShowDoubleAddr` arm is pointed
//! back at the old local `eval_haskell_show_double`), but it did NOT manifest
//! on any tracked real corpus/suite content — `corpus_report` and
//! `haskell_suite_differential` both compared clean before and after this
//! change, meaning no tracked fixture's `show`/`Show Double` call ever landed
//! on a bit pattern with this exact shape. Contrast with duplication survey
//! finding 5 (the `raise#`-in-a-Con-field trivial-field predicate, this
//! crate's `raise_con_field_trivial_differential.rs`), which item A's lane
//! found WAS being hit by real content.

use tidepool_codegen::host_fns::runtime_show_double_addr;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::types::{Literal, PrimOpKind};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

/// `PrimOp(ShowDoubleAddr, [Lit(LitDouble(bits))])` — the oracle side returns
/// this directly as a `Value::Lit(LitString)`; the JIT side compiles it as a
/// direct call to `runtime_show_double_addr`, exercised here without the full
/// compile pipeline since the host fn IS the JIT's real formatting logic (no
/// separate Cranelift reimplementation to bypass).
fn build_tree(bits: u64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    b.push(CoreFrame::Lit(Literal::LitDouble(bits)));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::ShowDoubleAddr,
        args: vec![0],
    });
    b.build()
}

/// Run the oracle's `ShowDoubleAddr` arm and decode the resulting `LitString`
/// (null-terminated, per `eval_at`) as a UTF-8 string.
fn oracle_show_double(bits: u64) -> String {
    let expr = build_tree(bits);
    let table = build_table_for_expr(&expr);
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let result = eval(&expr, &env, &mut heap);
    match result {
        Ok(Value::Lit(Literal::LitString(ref bytes))) => {
            let mut bytes = bytes.clone();
            if bytes.last() == Some(&0) {
                bytes.pop();
            }
            String::from_utf8(bytes).expect("oracle ShowDoubleAddr produced non-UTF8 bytes")
        }
        ref other => panic!("expected a LitString from ShowDoubleAddr, got {other:?}"),
    }
}

/// Call the JIT's real host fn directly and reclaim the leaked `CString` it
/// returns (documented contract, same as `proptest_host_arrays::host_show_double`).
fn jit_show_double(bits: u64) -> String {
    let p = runtime_show_double_addr(bits as i64);
    assert!((p as u64) >= 0x1000, "runtime_show_double_addr poisoned");
    // SAFETY: runtime_show_double_addr produced this via CString::into_raw;
    // from_raw reclaims ownership and frees on drop.
    let cs = unsafe { std::ffi::CString::from_raw(p as *mut std::os::raw::c_char) };
    cs.to_string_lossy().into_owned()
}

/// Scientific-notation doubles whose mantissa is exactly integral under
/// Rust's `{:e}` — the precise shape BUG-1 covers. Includes the smallest
/// subnormal (`5.0e-324`, the shrunk BUG-1 proptest witness) and both signs.
const SCIENTIFIC_NOTATION_CASES: &[f64] = &[
    1e8, 1e9, 1e10, 1e20, 1e-2, 1e-5, 1e100, 2e8, 3e10, 5e9, 7e7, -1e10, -1e100, -2e8,
];

#[test]
fn eval_and_jit_agree_on_scientific_notation_double_show() {
    for &d in SCIENTIFIC_NOTATION_CASES {
        let bits = d.to_bits();
        let oracle = oracle_show_double(bits);
        let jit = jit_show_double(bits);
        assert_eq!(
            oracle, jit,
            "eval/JIT ShowDoubleAddr disagreement for d={d:?} (bits={bits:#018x})"
        );
        // The specific BUG-1 shape: a scientific-notation mantissa must carry
        // a decimal point (Haskell `show` never emits e.g. "1e10").
        if let Some(epos) = jit.find('e') {
            assert!(
                jit[..epos].contains('.'),
                "scientific mantissa has no decimal point: {jit:?}"
            );
        }
    }
}

#[test]
fn eval_and_jit_agree_on_smallest_subnormal_show() {
    // bits=1: the smallest positive subnormal, the shrunk BUG-1 proptest
    // witness (`5.0e-324`).
    let bits = 1u64;
    assert_eq!(oracle_show_double(bits), jit_show_double(bits));
    assert_eq!(jit_show_double(bits), "5.0e-324");
}
