//! Value comparison utilities for property-based testing.
//!
//! Provides structural equality for interpreter `Value`s after deep-forcing,
//! and cross-backend comparison between interpreter `Value` and JIT heap objects.

use tidepool_eval::value::Value;
use tidepool_repr::{DataConId, Literal};

/// Compare two interpreter Values for structural equality.
///
/// Assumes both values have been deep-forced (no ThunkRef nodes).
/// Closures and JoinConts are skipped (any pair compares equal); ConFuns
/// compare by tag/arity/args; ByteArrays compare by CONTENT (the old
/// catch-all made eq(ba, ba.clone()) false — proptest_infra_selftest BUG-2).
///
/// Returns true if structurally equal, false otherwise.
///
/// Uses an explicit worklist instead of recursion so deeply nested values
/// (the whole point of lifting the depth-3 generator cap) cannot overflow the
/// host stack. Semantics are identical to the prior recursive version.
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
            (Value::Closure(..), Value::Closure(..)) => {}
            // JoinConts: similarly not comparable
            (Value::JoinCont(..), Value::JoinCont(..)) => {}
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

const MAX_HEAP_DEPTH: usize = 1000;
const MAX_CON_FIELDS: usize = 256;

/// Reconstruct an interpreter `Value` from a JIT heap object pointer.
///
/// Uses an explicit worklist (mirroring `tidepool_eval::eval::deep_force`)
/// instead of recursion, so deeply nested heap objects cannot overflow the
/// host stack. Forwarding pointers are followed per-visit, so GC moves that
/// occur while forcing a thunk in a sibling field are tolerated exactly as in
/// the prior recursive version.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject in the JIT nursery/heap.
/// `vmctx` must be valid and the nursery must still be alive.
pub unsafe fn heap_to_value(
    ptr: *const u8,
    vmctx: &mut tidepool_codegen::context::VMContext,
) -> Value {
    use tidepool_heap::layout;

    /// One unit of reconstruction work.
    enum Work {
        /// Decode the object at this pointer (carrying its depth).
        Visit(*const u8, usize),
        /// Pop `n` finished field values and assemble a `Con`.
        BuildCon(DataConId, usize),
    }

    let mut stack: Vec<Work> = vec![Work::Visit(ptr, 0)];
    let mut results: Vec<Value> = Vec::new();

    while let Some(w) = stack.pop() {
        match w {
            Work::Visit(mut ptr, depth) => {
                if depth > MAX_HEAP_DEPTH {
                    results.push(Value::ByteArray(std::sync::Arc::new(
                        std::sync::Mutex::new(vec![]),
                    )));
                    continue;
                }

                // Follow forwarding pointer if GC moved this object during a
                // previous thunk force (e.g., a sibling Con field).
                if layout::read_tag(ptr) == layout::TAG_FORWARDED {
                    ptr = *(ptr.add(8) as *const *const u8);
                }

                let tag = layout::read_tag(ptr);
                match tag {
                    layout::TAG_LIT => {
                        let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET);
                        let v = match layout::LitTag::from_byte(lit_tag) {
                            Some(layout::LitTag::Int) => Value::Lit(Literal::LitInt(
                                *(ptr.add(layout::LIT_VALUE_OFFSET) as *const i64),
                            )),
                            Some(layout::LitTag::Word) => Value::Lit(Literal::LitWord(
                                *(ptr.add(layout::LIT_VALUE_OFFSET) as *const u64),
                            )),
                            Some(layout::LitTag::Char) => {
                                let code = *(ptr.add(layout::LIT_VALUE_OFFSET) as *const u32);
                                Value::Lit(Literal::LitChar(
                                    char::from_u32(code).unwrap_or('\u{FFFD}'),
                                ))
                            }
                            Some(layout::LitTag::Float) => {
                                let bits = *(ptr.add(layout::LIT_VALUE_OFFSET) as *const u32);
                                Value::Lit(Literal::LitFloat(bits as u64))
                            }
                            Some(layout::LitTag::Double) => Value::Lit(Literal::LitDouble(
                                *(ptr.add(layout::LIT_VALUE_OFFSET) as *const u64),
                            )),
                            None => {
                                // JIT uses extended lit tags (5=String, 7=ByteArray)
                                Value::ByteArray(std::sync::Arc::new(std::sync::Mutex::new(vec![])))
                            }
                        };
                        results.push(v);
                    }
                    layout::TAG_CON => {
                        let con_tag = *(ptr.add(layout::CON_TAG_OFFSET) as *const u64);
                        let num_fields =
                            *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16) as usize;
                        let num_fields = num_fields.min(MAX_CON_FIELDS);
                        stack.push(Work::BuildCon(DataConId(con_tag), num_fields));
                        // Push fields in reverse so they are reconstructed in
                        // order and `BuildCon` pops them as field[0..n].
                        for i in (0..num_fields).rev() {
                            let field_ptr = *(ptr
                                .add(layout::CON_FIELDS_OFFSET + layout::FIELD_STRIDE * i)
                                as *const *const u8);
                            stack.push(Work::Visit(field_ptr, depth + 1));
                        }
                    }
                    layout::TAG_THUNK => {
                        // Force the thunk first, then decode the result.
                        let forced = tidepool_codegen::host_fns::heap_force(vmctx, ptr as *mut u8);
                        stack.push(Work::Visit(forced as *const u8, depth + 1));
                    }
                    layout::TAG_CLOSURE => {
                        // Can't reconstruct a closure — return a sentinel.
                        results.push(Value::Closure(
                            tidepool_eval::env::Env::new(),
                            tidepool_repr::VarId(0),
                            tidepool_repr::RecursiveTree {
                                nodes: vec![tidepool_repr::CoreFrame::Var(tidepool_repr::VarId(0))],
                            },
                        ));
                    }
                    _ => panic!("unknown heap tag: {}", tag),
                }
            }
            Work::BuildCon(tag, n) => {
                let start = results.len() - n;
                let fields = results.split_off(start);
                results.push(Value::Con(tag, fields));
            }
        }
    }

    results.pop().expect("heap_to_value: empty result stack")
}

/// Check if a value contains any closures (which can't be structurally compared
/// across backends).
pub fn contains_closure(val: &Value) -> bool {
    let mut stack: Vec<&Value> = vec![val];
    while let Some(v) = stack.pop() {
        match v {
            Value::Closure(..) => return true,
            Value::Con(_, fields) => stack.extend(fields.iter()),
            Value::ConFun(_, _, args) => stack.extend(args.iter()),
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let a = Value::Closure(env.clone(), tidepool_repr::VarId(0), expr.clone());
        let b = Value::Closure(env, tidepool_repr::VarId(1), expr);
        assert!(values_equal(&a, &b));
    }

    #[test]
    fn test_contains_closure() {
        assert!(!contains_closure(&Value::Lit(Literal::LitInt(42))));
        let env = tidepool_eval::env::Env::new();
        let expr = tidepool_repr::RecursiveTree {
            nodes: vec![tidepool_repr::CoreFrame::Var(tidepool_repr::VarId(0))],
        };
        let closure = Value::Closure(env, tidepool_repr::VarId(0), expr);
        assert!(contains_closure(&closure));
        let nested = Value::Con(DataConId(1), vec![closure]);
        assert!(contains_closure(&nested));
    }
}
