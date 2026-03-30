use crate::context::VMContext;
use crate::layout::{
    self, LIT_TAG_ADDR, LIT_TAG_ARRAY, LIT_TAG_BYTEARRAY, LIT_TAG_CHAR, LIT_TAG_DOUBLE,
    LIT_TAG_FLOAT, LIT_TAG_INT, LIT_TAG_SMALLARRAY, LIT_TAG_STRING, LIT_TAG_WORD,
};
use std::fmt;
use tidepool_eval::value::Value;
use tidepool_heap::layout as heap_layout;
use tidepool_repr::{DataConId, Literal};

#[derive(Debug)]
pub enum BridgeError {
    UnexpectedHeapTag(u8),
    UnexpectedLitTag(u8),
    NullPointer,
    NurseryExhausted,
    TooManyFields { count: usize },
    DataTooLarge { len: usize },
    TooDeep,
    UnevaluatedThunk,
    BlackHole,
    UnknownThunkState(u8),
    InternalError(String),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BridgeError::UnexpectedHeapTag(t) => write!(f, "unexpected heap tag: {}", t),
            BridgeError::UnexpectedLitTag(t) => write!(f, "unexpected lit tag: {}", t),
            BridgeError::NullPointer => write!(f, "null pointer"),
            BridgeError::NurseryExhausted => write!(f, "nursery exhausted"),
            BridgeError::TooManyFields { count } => write!(f, "too many Con fields: {}", count),
            BridgeError::DataTooLarge { len } => write!(f, "data too large: {} bytes", len),
            BridgeError::TooDeep => write!(f, "heap structure too deep (>10000 levels)"),
            BridgeError::UnevaluatedThunk => write!(f, "unevaluated thunk"),
            BridgeError::BlackHole => write!(f, "blackhole (thunk forcing itself)"),
            BridgeError::UnknownThunkState(state) => write!(f, "unknown thunk state: {}", state),
            BridgeError::InternalError(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for BridgeError {}

/// Convert a heap-allocated object to a Value.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject allocated by the JIT nursery.
pub unsafe fn heap_to_value(ptr: *const u8) -> Result<Value, BridgeError> {
    // SAFETY: Caller guarantees ptr is a valid HeapObject from the JIT nursery.
    heap_to_value_inner(ptr, 0, std::ptr::null_mut())
}

/// Convert a heap-allocated object to a Value, forcing any unevaluated thunks
/// encountered during traversal.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject allocated by the JIT nursery.
/// `vmctx` must point to a valid VMContext (required for forcing thunks).
pub unsafe fn heap_to_value_forcing(
    ptr: *const u8,
    vmctx: *mut VMContext,
) -> Result<Value, BridgeError> {
    // SAFETY: Caller guarantees ptr is a valid HeapObject and vmctx is a valid VMContext.
    heap_to_value_inner(ptr, 0, vmctx)
}

const MAX_DEPTH: usize = 10_000;
const MAX_FIELDS: usize = 1024;
const MAX_DATA_SIZE: usize = 64 * 1024 * 1024; // 64MB

unsafe fn heap_to_value_inner(
    ptr: *const u8,
    depth: usize,
    vmctx: *mut VMContext,
) -> Result<Value, BridgeError> {
    // SAFETY: ptr is a valid HeapObject from the JIT nursery (checked non-null below).
    // All field reads use known layout offsets. Recursion depth is bounded by MAX_DEPTH.
    if ptr.is_null() {
        return Err(BridgeError::NullPointer);
    }
    if depth > MAX_DEPTH {
        return Err(BridgeError::TooDeep);
    }

    let tag = *ptr;
    match tag {
        t if t == layout::TAG_LIT => {
            let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET as usize) as i64;
            let raw_value = *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64);

            match lit_tag {
                x if x == LIT_TAG_INT => Ok(Value::Lit(Literal::LitInt(raw_value))),
                x if x == LIT_TAG_WORD => Ok(Value::Lit(Literal::LitWord(raw_value as u64))),
                x if x == LIT_TAG_CHAR => Ok(Value::Lit(Literal::LitChar(
                    char::from_u32(raw_value as u32).unwrap_or('\0'),
                ))),
                x if x == LIT_TAG_FLOAT => Ok(Value::Lit(Literal::LitFloat(raw_value as u64))),
                x if x == LIT_TAG_DOUBLE => Ok(Value::Lit(Literal::LitDouble(raw_value as u64))),
                x if x == LIT_TAG_STRING => {
                    // LitString: value is pointer to [len: u64][bytes...]
                    // Use read_unaligned because JIT data sections may not be 8-byte aligned
                    let data_ptr = raw_value as *const u8;
                    if data_ptr.is_null() {
                        return Err(BridgeError::NullPointer);
                    }
                    let len = std::ptr::read_unaligned(data_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let bytes_ptr = data_ptr.add(8);
                    let bytes = std::slice::from_raw_parts(bytes_ptr, len).to_vec();
                    Ok(Value::Lit(Literal::LitString(bytes)))
                }
                x if x == LIT_TAG_ADDR => {
                    // Addr# — intermediate value, shouldn't normally be a final result.
                    // Wrap as empty LitString as graceful fallback.
                    Ok(Value::Lit(Literal::LitString(vec![])))
                }
                x if x == LIT_TAG_BYTEARRAY => {
                    // ByteArray# — raw pointer to [len: u64][bytes...]
                    let ba_ptr = raw_value as *const u8;
                    if ba_ptr.is_null() {
                        return Ok(Value::ByteArray(std::sync::Arc::new(
                            std::sync::Mutex::new(vec![]),
                        )));
                    }
                    let len = std::ptr::read_unaligned(ba_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let bytes_ptr = ba_ptr.add(8);
                    let bytes = std::slice::from_raw_parts(bytes_ptr, len).to_vec();
                    Ok(Value::ByteArray(std::sync::Arc::new(
                        std::sync::Mutex::new(bytes),
                    )))
                }
                x if x == LIT_TAG_SMALLARRAY || x == LIT_TAG_ARRAY => {
                    // SmallArray# (8) / Array# (9) — boxed pointer arrays
                    // Layout: [u64 length][ptr0][ptr1]...[ptrN-1]
                    let arr_ptr = raw_value as *const u8;
                    if arr_ptr.is_null() {
                        return Ok(Value::Con(DataConId(0), vec![]));
                    }
                    let len = std::ptr::read_unaligned(arr_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let elems: Vec<_> = (0..len)
                        .map(|i| {
                            let elem_ptr = *(arr_ptr.add(8 + 8 * i) as *const *const u8);
                            heap_to_value_inner(elem_ptr, depth + 1, vmctx)
                        })
                        .collect::<Result<_, _>>()?;
                    // Return as a generic Con with fields — the renderer will
                    // see the constructor names from the wrapping Con objects
                    // (e.g., Vector's Array constructor wraps this)
                    Ok(Value::Con(DataConId(0), elems))
                }
                other => Err(BridgeError::UnexpectedLitTag(other as u8)),
            }
        }
        t if t == layout::TAG_CON => {
            let con_tag = *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64);
            let num_fields =
                *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) as usize;
            if num_fields > MAX_FIELDS {
                return Err(BridgeError::TooManyFields { count: num_fields });
            }
            let fields: Vec<_> = (0..num_fields)
                .map(|i| {
                    let field_ptr =
                        *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *const *const u8);
                    heap_to_value_inner(field_ptr, depth + 1, vmctx)
                })
                .collect::<Result<_, _>>()?;
            Ok(Value::Con(DataConId(con_tag), fields))
        }
        t if t == layout::TAG_THUNK => {
            let state = unsafe { *ptr.add(layout::THUNK_STATE_OFFSET as usize) };
            match state {
                layout::THUNK_EVALUATED => {
                    // Follow indirection pointer to the WHNF result
                    let target = unsafe {
                        *(ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *const *const u8)
                    };
                    heap_to_value_inner(target, depth + 1, vmctx)
                }
                _ if !vmctx.is_null() => {
                    // Force the thunk via heap_force when vmctx is available
                    let forced = crate::host_fns::heap_force(vmctx, ptr as *mut u8);
                    if !forced.is_null() && !std::ptr::eq(forced, ptr) {
                        heap_to_value_inner(forced as *const u8, depth + 1, vmctx)
                    } else {
                        Err(BridgeError::UnevaluatedThunk)
                    }
                }
                layout::THUNK_UNEVALUATED => Err(BridgeError::UnevaluatedThunk),
                layout::THUNK_BLACKHOLE => Err(BridgeError::BlackHole),
                _ => Err(BridgeError::UnknownThunkState(state)),
            }
        }
        t if t == layout::TAG_CLOSURE => {
            // Unevaluated closure — return as opaque Value.
            // This can happen when Array# elements haven't been forced.
            // We represent it as a dummy Closure with empty env and body.
            use tidepool_eval::env::Env;
            use tidepool_repr::{CoreExpr, CoreFrame, VarId};
            let expr = CoreExpr {
                nodes: vec![CoreFrame::Var(VarId(0))],
            };
            Ok(Value::Closure(Env::new(), VarId(0), expr))
        }
        other => Err(BridgeError::UnexpectedHeapTag(other)),
    }
}

/// Convert a Value to a heap-allocated object via VMContext bump allocation.
///
/// # Safety
///
/// `vmctx` must point to a valid VMContext with sufficient nursery space.
pub unsafe fn value_to_heap(val: &Value, vmctx: &mut VMContext) -> Result<*mut u8, BridgeError> {
    // SAFETY: Caller guarantees vmctx has a live nursery with sufficient space.
    // All writes use known layout offsets within bump-allocated nursery memory.
    match val {
        Value::Lit(lit) => {
            let ptr = bump_alloc_from_vmctx(vmctx, layout::LIT_TOTAL_SIZE as usize);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }

            match lit {
                Literal::LitInt(n) => {
                    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_INT as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *n;
                }
                Literal::LitWord(n) => {
                    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_WORD as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *n as i64;
                }
                Literal::LitChar(c) => {
                    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_CHAR as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *c as i64;
                }
                Literal::LitFloat(bits) => {
                    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_FLOAT as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *bits as i64;
                }
                Literal::LitDouble(bits) => {
                    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_DOUBLE as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *bits as i64;
                }
                Literal::LitString(bytes) => {
                    // Allocate string data: [len: u64][bytes...]
                    let data_size = 8 + bytes.len();
                    let data_ptr = bump_alloc_from_vmctx(vmctx, data_size);
                    if data_ptr.is_null() {
                        // Roll back the Lit object allocation to avoid dead space in nursery
                        vmctx.alloc_ptr = ptr;
                        return Err(BridgeError::NurseryExhausted);
                    }
                    *(data_ptr as *mut u64) = bytes.len() as u64;
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr.add(8), bytes.len());

                    // Only write the header once we're sure all allocations succeeded
                    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_STRING as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = data_ptr as i64;
                }
            }
            Ok(ptr)
        }
        Value::Con(id, fields) => {
            // Recursively convert fields first
            let mut field_ptrs = Vec::with_capacity(fields.len());
            for f in fields {
                field_ptrs.push(value_to_heap(f, vmctx)?);
            }

            let size = 24 + 8 * fields.len();
            let ptr = bump_alloc_from_vmctx(vmctx, size);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            heap_layout::write_header(ptr, layout::TAG_CON, size as u16);

            *(ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = id.0;
            *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = fields.len() as u16;

            for (i, fp) in field_ptrs.into_iter().enumerate() {
                *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *mut *mut u8) = fp;
            }
            Ok(ptr)
        }
        Value::ByteArray(bytes) => {
            // ByteArray# stored as Lit with tag=7 (LIT_TAG_BYTEARRAY), value = ptr to [len: u64][bytes...]
            // The byte data buffer must be allocated outside the GC nursery (via malloc)
            // because GC doesn't track the interior pointer from the Lit wrapper to the
            // data buffer. Using bump_alloc would place data in the nursery; after a
            // Cheney copy, the Lit's data_ptr would point to stale fromspace memory.
            let bytes = bytes
                .lock()
                .map_err(|e| BridgeError::InternalError(format!("mutex poisoned: {e}")))?;
            let data_ptr = crate::host_fns::runtime_new_byte_array(bytes.len() as i64) as *mut u8;
            if data_ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr.add(8), bytes.len());

            let ptr = bump_alloc_from_vmctx(vmctx, layout::LIT_TOTAL_SIZE as usize);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
            *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_BYTEARRAY as u8;
            *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = data_ptr as i64;
            Ok(ptr)
        }
        _ => Err(BridgeError::UnexpectedHeapTag(255)),
    }
}

/// Bump-allocate from VMContext. Returns null if nursery is exhausted.
///
/// # Safety
///
/// `vmctx` must point to a valid VMContext with a live nursery.
pub unsafe fn bump_alloc_from_vmctx(vmctx: &mut VMContext, size: usize) -> *mut u8 {
    // SAFETY: Caller guarantees vmctx points to a valid VMContext with a live nursery.
    // alloc_ptr and alloc_limit delimit the available nursery region.
    // Align to 8 bytes
    let aligned_size = (size + 7) & !7;
    let ptr = vmctx.alloc_ptr;
    let new_ptr = ptr.add(aligned_size);
    if new_ptr as *const u8 > vmctx.alloc_limit {
        return std::ptr::null_mut();
    }
    vmctx.alloc_ptr = new_ptr;
    ptr
}

#[cfg(test)]
mod tests {
    // SAFETY: All unsafe blocks in tests call value_to_heap/heap_to_value with
    // nursery-backed VMContexts created by setup_vmctx. The nursery is kept alive
    // for the duration of each test, ensuring all heap pointers remain valid.
    use super::*;
    use crate::nursery::Nursery;
    use std::sync::{Arc, Mutex};
    use tidepool_repr::{DataConId, Literal};

    extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

    fn setup_vmctx(size: usize) -> (Nursery, VMContext) {
        let mut nursery = Nursery::new(size);
        let vmctx = nursery.make_vmctx(mock_gc_trigger);
        (nursery, vmctx)
    }

    #[test]
    fn test_lit_int_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitInt(42));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitInt(n)) = back else {
                panic!("Expected LitInt, got {:?}", back);
            };
            assert_eq!(n, 42);
        }
    }

    #[test]
    fn test_lit_word_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitWord(123));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitWord(n)) = back else {
                panic!("Expected LitWord, got {:?}", back);
            };
            assert_eq!(n, 123);
        }
    }

    #[test]
    fn test_lit_char_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitChar('λ'));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitChar(c)) = back else {
                panic!("Expected LitChar, got {:?}", back);
            };
            assert_eq!(c, 'λ');
        }
    }

    #[test]
    fn test_lit_double_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitDouble(f64::to_bits(1.2345678)));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitDouble(bits)) = back else {
                panic!("Expected LitDouble, got {:?}", back);
            };
            assert_eq!(f64::from_bits(bits), 1.2345678);
        }
    }

    #[test]
    fn test_lit_string_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bytes = b"hello world".to_vec();
        let val = Value::Lit(Literal::LitString(bytes.clone()));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitString(b)) = back else {
                panic!("Expected LitString, got {:?}", back);
            };
            assert_eq!(b, bytes);
        }
    }

    #[test]
    fn test_con_no_fields() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Con(DataConId(42), vec![]);
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Con(id, fields) = back else {
                panic!("Expected Con, got {:?}", back);
            };
            assert_eq!(id.0, 42);
            assert!(fields.is_empty());
        }
    }

    #[test]
    fn test_con_lit_fields() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(Literal::LitInt(10)),
                Value::Lit(Literal::LitChar('a')),
            ],
        );
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Con(id, fields) = back else {
                panic!("Expected Con, got {:?}", back);
            };
            assert_eq!(id.0, 1);
            assert_eq!(fields.len(), 2);
            match (&fields[0], &fields[1]) {
                (Value::Lit(Literal::LitInt(10)), Value::Lit(Literal::LitChar('a'))) => (),
                _ => panic!("Expected [LitInt(10), LitChar('a')], got {:?}", fields),
            }
        }
    }

    #[test]
    fn test_con_nested() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        // Just (I# 42)
        let inner = Value::Con(DataConId(2), vec![Value::Lit(Literal::LitInt(42))]);
        let val = Value::Con(DataConId(1), vec![inner]);
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");

            let Value::Con(id, fields) = back else {
                panic!("Expected Con");
            };
            assert_eq!(id.0, 1);
            assert_eq!(fields.len(), 1);
            let Value::Con(id2, fields2) = &fields[0] else {
                panic!("Expected nested Con");
            };
            assert_eq!(id2.0, 2);
            assert_eq!(fields2.len(), 1);
            let Value::Lit(Literal::LitInt(n)) = &fields2[0] else {
                panic!("Expected LitInt");
            };
            assert_eq!(*n, 42);
        }
    }

    #[test]
    fn test_byte_array_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let data = vec![1, 2, 3, 4, 5];
        let val = Value::ByteArray(Arc::new(Mutex::new(data.clone())));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::ByteArray(ba) = back else {
                panic!("Expected ByteArray, got {:?}", back);
            };
            assert_eq!(*ba.lock().unwrap(), data);
        }
    }

    #[test]
    fn test_bump_alloc_alignment() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        unsafe {
            let p1 = bump_alloc_from_vmctx(&mut vmctx, 1);
            let p2 = bump_alloc_from_vmctx(&mut vmctx, 1);
            assert_eq!(p1 as usize % 8, 0);
            assert_eq!(p2 as usize % 8, 0);
            assert_eq!(p2 as usize - p1 as usize, 8);
        }
    }

    #[test]
    fn test_bump_alloc_bounds() {
        let (_nursery, mut vmctx) = setup_vmctx(16);
        unsafe {
            let p1 = bump_alloc_from_vmctx(&mut vmctx, 8);
            assert!(!p1.is_null());
            let p2 = bump_alloc_from_vmctx(&mut vmctx, 8);
            assert!(!p2.is_null());
            let p3 = bump_alloc_from_vmctx(&mut vmctx, 1);
            assert!(p3.is_null());
        }
    }

    #[test]
    fn test_lit_float_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bits = f32::to_bits(1.23f32) as u64;
        let val = Value::Lit(Literal::LitFloat(bits));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitFloat(b)) = back else {
                panic!("Expected LitFloat, got {:?}", back);
            };
            assert_eq!(b, bits);
        }
    }

    #[test]
    fn test_null_pointer_error() {
        let result = unsafe { heap_to_value(std::ptr::null()) };
        assert!(matches!(result, Err(BridgeError::NullPointer)));
    }

    #[test]
    fn test_invalid_heap_tag() {
        let buf = [0xFFu8; 32];
        let result = unsafe { heap_to_value(buf.as_ptr()) };
        assert!(matches!(result, Err(BridgeError::UnexpectedHeapTag(0xFF))));
    }
}
