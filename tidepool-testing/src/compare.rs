//! Value comparison utilities for property-based testing.
//!
//! Provides structural equality for interpreter `Value`s after deep-forcing,
//! and cross-backend comparison between interpreter `Value` and JIT heap objects.

use tidepool_eval::value::Value;
use tidepool_repr::{DataConId, Literal};

/// Compare two interpreter Values for structural equality.
///
/// Assumes both values have been deep-forced (no ThunkRef nodes).
/// Closures, JoinConts, ConFuns, and ByteArrays are considered equal
/// only to themselves (they're skipped/not comparable structurally).
///
/// Returns true if structurally equal, false otherwise.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(la), Value::Lit(lb)) => lits_equal(la, lb),
        (Value::Con(tag_a, fields_a), Value::Con(tag_b, fields_b)) => {
            tag_a == tag_b
                && fields_a.len() == fields_b.len()
                && fields_a
                    .iter()
                    .zip(fields_b.iter())
                    .all(|(fa, fb)| values_equal(fa, fb))
        }
        // Closures: can't structurally compare, so treat as equal if both are closures
        (Value::Closure(..), Value::Closure(..)) => true,
        // JoinConts: similarly not comparable
        (Value::JoinCont(..), Value::JoinCont(..)) => true,
        // ConFun: compare tag and accumulated args
        (Value::ConFun(tag_a, arity_a, args_a), Value::ConFun(tag_b, arity_b, args_b)) => {
            tag_a == tag_b
                && arity_a == arity_b
                && args_a.len() == args_b.len()
                && args_a
                    .iter()
                    .zip(args_b.iter())
                    .all(|(fa, fb)| values_equal(fa, fb))
        }
        _ => false,
    }
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
/// # Safety
///
/// `ptr` must point to a valid HeapObject in the JIT nursery/heap.
/// `vmctx` must be valid and the nursery must still be alive.
pub unsafe fn heap_to_value(
    ptr: *const u8,
    vmctx: &mut tidepool_codegen::context::VMContext,
) -> Value {
    heap_to_value_inner(ptr, vmctx, 0)
}

unsafe fn heap_to_value_inner(
    ptr: *const u8,
    vmctx: &mut tidepool_codegen::context::VMContext,
    depth: usize,
) -> Value {
    use tidepool_heap::layout;

    if depth > MAX_HEAP_DEPTH {
        return Value::ByteArray(std::sync::Arc::new(std::sync::Mutex::new(vec![])));
    }

    // Follow forwarding pointer if GC moved this object during a previous
    // recursive call (e.g., thunk forcing in a sibling Con field).
    let mut ptr = ptr;
    if layout::read_tag(ptr) == layout::TAG_FORWARDED {
        ptr = *(ptr.add(8) as *const *const u8);
    }

    let tag = layout::read_tag(ptr);
    match tag {
        layout::TAG_LIT => {
            let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET);
            match layout::LitTag::from_byte(lit_tag) {
                Some(layout::LitTag::Int) => Value::Lit(Literal::LitInt(
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *const i64),
                )),
                Some(layout::LitTag::Word) => Value::Lit(Literal::LitWord(
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *const u64),
                )),
                Some(layout::LitTag::Char) => {
                    let code = *(ptr.add(layout::LIT_VALUE_OFFSET) as *const u32);
                    Value::Lit(Literal::LitChar(char::from_u32(code).unwrap_or('\u{FFFD}')))
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
            }
        }
        layout::TAG_CON => {
            let con_tag = *(ptr.add(layout::CON_TAG_OFFSET) as *const u64);
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16) as usize;
            let num_fields = num_fields.min(MAX_CON_FIELDS);
            let fields = (0..num_fields)
                .map(|i| {
                    let field_ptr = *(ptr.add(layout::CON_FIELDS_OFFSET + layout::FIELD_STRIDE * i)
                        as *const *const u8);
                    heap_to_value_inner(field_ptr, vmctx, depth + 1)
                })
                .collect();
            Value::Con(DataConId(con_tag), fields)
        }
        layout::TAG_THUNK => {
            // Force the thunk first, then read the result
            let forced = tidepool_codegen::host_fns::heap_force(vmctx, ptr as *mut u8);
            heap_to_value_inner(forced as *const u8, vmctx, depth + 1)
        }
        layout::TAG_CLOSURE => {
            // Can't reconstruct a closure — return a sentinel
            Value::Closure(
                tidepool_eval::env::Env::new(),
                tidepool_repr::VarId(0),
                tidepool_repr::RecursiveTree {
                    nodes: vec![tidepool_repr::CoreFrame::Var(tidepool_repr::VarId(0))],
                },
            )
        }
        _ => panic!("unknown heap tag: {}", tag),
    }
}

/// Check if a value contains any closures (which can't be structurally compared
/// across backends).
pub fn contains_closure(val: &Value) -> bool {
    match val {
        Value::Closure(..) => true,
        Value::Con(_, fields) => fields.iter().any(contains_closure),
        Value::ConFun(_, _, args) => args.iter().any(contains_closure),
        _ => false,
    }
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
