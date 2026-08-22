//! Value comparison utilities for property-based testing.
//!
//! Provides structural equality for interpreter `Value`s after deep-forcing,
//! and cross-backend comparison between interpreter `Value` and JIT heap objects.

use tidepool_eval::value::Value;
use tidepool_repr::Literal;

/// Compare two interpreter Values for structural equality.
///
/// Assumes both values have been deep-forced (no ThunkRef nodes).
///
/// Uses an explicit worklist instead of recursion so deeply nested values
/// cannot overflow the host stack.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((x, y)) = stack.pop() {
        match (x, y) {
            (Value::Lit(la), Value::Lit(lb)) => {
                if !lits_equal(la, lb) {
                    return false;
                }
            }
            (Value::Con(tag_a, fields_a), Value::Con(tag_b, fields_b)) => {
                if tag_a != tag_b || fields_a.len() != fields_b.len() {
                    return false;
                }
                for pair in fields_a.iter().zip(fields_b.iter()) {
                    stack.push(pair);
                }
            }
            // Closures: can't structurally compare, so treat as equal if both are closures
            (Value::Closure { .. }, Value::Closure { .. }) => {}
            // JoinConts: similarly not comparable
            (Value::JoinCont { .. }, Value::JoinCont { .. }) => {}
            // ConFun: compare tag and accumulated args
            (Value::ConFun(tag_a, arity_a, args_a), Value::ConFun(tag_b, arity_b, args_b)) => {
                if tag_a != tag_b || arity_a != arity_b || args_a.len() != args_b.len() {
                    return false;
                }
                for pair in args_a.iter().zip(args_b.iter()) {
                    stack.push(pair);
                }
            }
            // ByteArray: compare by content (BUG-2: the catch-all violated
            // reflexivity — eq(ba, ba.clone()) was false). Arc::ptr_eq first:
            // a cloned Value shares the SAME Mutex, and locking it twice in
            // one thread deadlocks (std Mutex is not reentrant).
            (Value::ByteArray(ba), Value::ByteArray(bb)) => {
                if !std::sync::Arc::ptr_eq(ba, bb) {
                    let xa = ba.lock().unwrap_or_else(|e| e.into_inner());
                    let xb = bb.lock().unwrap_or_else(|e| e.into_inner());
                    if *xa != *xb {
                        return false;
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

/// Compare two Literals for equality, handling NaN for floating point.
fn lits_equal(a: &Literal, b: &Literal) -> bool {
    match (a, b) {
        (Literal::LitInt(x), Literal::LitInt(y)) => x == y,
        (Literal::LitWord(x), Literal::LitWord(y)) => x == y,
        (Literal::LitChar(x), Literal::LitChar(y)) => x == y,
        (Literal::LitString(x), Literal::LitString(y)) => x == y,
        (Literal::LitFloat(x), Literal::LitFloat(y)) => {
            let fx = f32::from_bits(*x as u32);
            let fy = f32::from_bits(*y as u32);
            // NaN == NaN for testing purposes
            (fx.is_nan() && fy.is_nan()) || x == y
        }
        (Literal::LitDouble(x), Literal::LitDouble(y)) => {
            let fx = f64::from_bits(*x);
            let fy = f64::from_bits(*y);
            // NaN == NaN for testing purposes
            (fx.is_nan() && fy.is_nan()) || x == y
        }
        _ => false,
    }
}

/// Assert that two deep-forced interpreter values are structurally equal.
/// Panics with a detailed message on mismatch.
pub fn assert_values_eq(a: &Value, b: &Value) {
    if !values_equal(a, b) {
        panic!("Value mismatch:\n  left:  {}\n  right: {}", a, b,);
    }
}

/// Reconstruct an interpreter `Value` from a JIT heap object pointer.
///
/// Thin adapter over the canonical `tidepool_codegen::heap_bridge` decoder —
/// forcing (thunks along the way are resolved) and closure-tolerant (a
/// `TAG_CLOSURE` object becomes `heap_bridge::CLOSURE_SENTINEL` rather than an
/// error, since a differential comparison needs "opaque but present", not a
/// hard failure). This crate used to carry its own full copy of the decoder;
/// the only real difference was cosmetic (a synthetic `Value::Closure`
/// placeholder instead of the sentinel `Con`), so [`contains_closure`]
/// recognizes both.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject in the JIT nursery/heap.
/// `vmctx` must be valid and the nursery must still be alive.
pub unsafe fn heap_to_value(
    ptr: *const u8,
    vmctx: &mut tidepool_codegen::context::VMContext,
) -> Value {
    let vmctx_ptr: *mut tidepool_codegen::context::VMContext = vmctx;
    tidepool_codegen::heap_bridge::heap_to_value_forcing_tolerant(ptr, vmctx_ptr)
        .unwrap_or_else(|e| panic!("heap_to_value: bridge error: {e}"))
}

/// Check if a value contains any closures (which can't be structurally compared
/// across backends). Recognizes both the oracle's native `Value::Closure` and
/// the bridge's `CLOSURE_SENTINEL` placeholder Con (what a JIT-heap-decoded
/// closure looks like after [`heap_to_value`]).
pub fn contains_closure(val: &Value) -> bool {
    let mut stack: Vec<&Value> = vec![val];
    while let Some(v) = stack.pop() {
        match v {
            Value::Closure { .. } => return true,
            Value::Con(tag, fields) => {
                if *tag == tidepool_codegen::heap_bridge::CLOSURE_SENTINEL {
                    return true;
                }
                stack.extend(fields.iter());
            }
            Value::ConFun(_, _, args) => stack.extend(args.iter()),
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConId;

    #[test]
    fn test_lit_equality() {
        assert!(values_equal(
            &Value::Lit(Literal::LitInt(42)),
            &Value::Lit(Literal::LitInt(42))
        ));
        assert!(!values_equal(
            &Value::Lit(Literal::LitInt(42)),
            &Value::Lit(Literal::LitInt(43))
        ));
    }

    #[test]
    fn test_con_equality() {
        let a = Value::Con(DataConId(1), vec![Value::Lit(Literal::LitInt(10))]);
        let b = Value::Con(DataConId(1), vec![Value::Lit(Literal::LitInt(10))]);
        let c = Value::Con(DataConId(1), vec![Value::Lit(Literal::LitInt(20))]);
        let d = Value::Con(DataConId(2), vec![Value::Lit(Literal::LitInt(10))]);
        assert!(values_equal(&a, &b));
        assert!(!values_equal(&a, &c));
        assert!(!values_equal(&a, &d));
    }

    #[test]
    fn test_nan_equality() {
        let nan_a = Value::Lit(Literal::LitDouble(f64::NAN.to_bits()));
        let nan_b = Value::Lit(Literal::LitDouble(f64::NAN.to_bits()));
        assert!(values_equal(&nan_a, &nan_b));
    }

    #[test]
    fn test_closure_equality() {
        // Two closures are considered equal (not structurally comparable)
        let env = tidepool_eval::env::Env::new();
        let expr = tidepool_repr::RecursiveTree {
            nodes: vec![tidepool_repr::CoreFrame::Var(tidepool_repr::VarId(0))],
        };
        let a = Value::Closure {
            env: env.clone(),
            binder: tidepool_repr::VarId(0),
            body: expr.clone(),
        };
        let b = Value::Closure {
            env,
            binder: tidepool_repr::VarId(1),
            body: expr,
        };
        assert!(values_equal(&a, &b));
    }

    // Regression (#334): the compare heap reader must return the REAL backing
    // bytes for pointer-carrying heap lits (LitString tag 5 / ByteArray tag 7),
    // not the old empty-ByteArray placeholder — otherwise the differential
    // harness reports false `<ByteArray# len=0>` divergences against eval's real
    // bytes. Builds the heap objects through the canonical
    // `heap_bridge::value_to_heap` and reads them back through this reader.
    extern "C" fn mock_gc_trigger(_vmctx: *mut tidepool_codegen::context::VMContext) {}

    #[test]
    fn jit_heap_litstring_roundtrips_bytes_through_compare_reader() {
        let mut nursery = tidepool_codegen::nursery::Nursery::new(4096);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        let s = b"hello #334".to_vec();
        let val = Value::Lit(Literal::LitString(s.clone()));
        unsafe {
            let ptr = tidepool_codegen::heap_bridge::value_to_heap(&val, &mut vmctx)
                .expect("value_to_heap LitString");
            let back = heap_to_value(ptr, &mut vmctx);
            match &back {
                Value::Lit(Literal::LitString(bytes)) => assert_eq!(*bytes, s),
                other => panic!("expected non-empty LitString, got {other}"),
            }
        }
    }

    #[test]
    fn jit_heap_bytearray_roundtrips_bytes_through_compare_reader() {
        let mut nursery = tidepool_codegen::nursery::Nursery::new(4096);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        let b = vec![0u8, 1, 2, 250, 255];
        let val = Value::ByteArray(std::sync::Arc::new(std::sync::Mutex::new(b.clone())));
        unsafe {
            let ptr = tidepool_codegen::heap_bridge::value_to_heap(&val, &mut vmctx)
                .expect("value_to_heap ByteArray");
            let back = heap_to_value(ptr, &mut vmctx);
            match &back {
                Value::ByteArray(arc) => {
                    let got = arc.lock().unwrap_or_else(|e| e.into_inner());
                    assert_eq!(*got, b, "ByteArray must carry real bytes, not placeholder");
                }
                other => panic!("expected non-empty ByteArray, got {other}"),
            }
        }
    }

    #[test]
    fn jit_heap_closure_becomes_sentinel_through_compare_reader() {
        let mut nursery = tidepool_codegen::nursery::Nursery::new(4096);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        unsafe {
            let ptr = tidepool_codegen::heap_bridge::bump_alloc_from_vmctx(&mut vmctx, 8);
            *ptr = tidepool_heap::layout::TAG_CLOSURE;
            let back = heap_to_value(ptr, &mut vmctx);
            assert!(
                matches!(&back, Value::Con(id, fields) if *id == tidepool_codegen::heap_bridge::CLOSURE_SENTINEL && fields.is_empty()),
                "expected a childless CLOSURE_SENTINEL Con, got {back:?}"
            );
            assert!(
                contains_closure(&back),
                "contains_closure must recognize the bridge's sentinel Con"
            );
        }
    }

    #[test]
    fn test_contains_closure() {
        assert!(!contains_closure(&Value::Lit(Literal::LitInt(42))));
        let env = tidepool_eval::env::Env::new();
        let expr = tidepool_repr::RecursiveTree {
            nodes: vec![tidepool_repr::CoreFrame::Var(tidepool_repr::VarId(0))],
        };
        let closure = Value::Closure {
            env,
            binder: tidepool_repr::VarId(0),
            body: expr,
        };
        assert!(contains_closure(&closure));
        let nested = Value::Con(DataConId(1), vec![closure]);
        assert!(contains_closure(&nested));
    }
}
