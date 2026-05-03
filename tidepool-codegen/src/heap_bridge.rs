use crate::context::VMContext;
use crate::layout::{
    self, LIT_TAG_ADDR, LIT_TAG_ARRAY, LIT_TAG_BYTEARRAY, LIT_TAG_CHAR, LIT_TAG_DOUBLE,
    LIT_TAG_FLOAT, LIT_TAG_INT, LIT_TAG_SMALLARRAY, LIT_TAG_STRING, LIT_TAG_WORD,
};
use tidepool_eval::value::Value;
use tidepool_heap::layout as heap_layout;
use tidepool_repr::{DataConId, Literal};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("unexpected heap tag: {0}")]
    UnexpectedHeapTag(u8),
    #[error("unexpected lit tag: {0}")]
    UnexpectedLitTag(u8),
    #[error("null pointer")]
    NullPointer,
    #[error("nursery exhausted")]
    NurseryExhausted,
    #[error("too many Con fields: {count}")]
    TooManyFields { count: usize },
    #[error("data too large: {len} bytes")]
    DataTooLarge { len: usize },
    #[error("heap structure too deep (>10000 levels)")]
    TooDeep,
    #[error("unevaluated thunk")]
    UnevaluatedThunk,
    #[error("blackhole (thunk forcing itself)")]
    BlackHole,
    #[error("unknown thunk state: {0}")]
    UnknownThunkState(u8),
    #[error("internal error: {0}")]
    InternalError(String),
}

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
/// Maximum number of fields the read-side decoder will accept on a single
/// `Con` heap object. The poison buffer in `host_fns` must be large enough
/// to absorb a worst-case Con write at this arity (see
/// `host_fns::POISON_BUF_SIZE` and the compile-time assertion there).
pub(crate) const MAX_FIELDS: usize = 1024;
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
                x if x == LIT_TAG_CHAR => {
                    // core-shapes.md §1: Char lit must hold valid Unicode codepoint.
                    // Fallback to \0 if invalid.
                    let c = char::from_u32(raw_value as u32);
                    #[cfg(debug_assertions)]
                    if c.is_none() {
                        eprintln!("[heap_bridge] diagnostic: invalid Unicode codepoint {:#x} in Char lit; falling back to \\0", raw_value);
                    }
                    Ok(Value::Lit(Literal::LitChar(c.unwrap_or('\0'))))
                }
                x if x == LIT_TAG_FLOAT => Ok(Value::Lit(Literal::LitFloat(raw_value as u64))),
                x if x == LIT_TAG_DOUBLE => Ok(Value::Lit(Literal::LitDouble(raw_value as u64))),
                x if x == LIT_TAG_STRING => {
                    // LitString# — raw pointer to [len: u64][bytes...]
                    let str_ptr = raw_value as *const u8;
                    if str_ptr.is_null() {
                        return Err(BridgeError::NullPointer);
                    }
                    let len = std::ptr::read_unaligned(str_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let bytes_ptr = str_ptr.add(8);
                    let bytes = std::slice::from_raw_parts(bytes_ptr, len).to_vec();
                    Ok(Value::Lit(Literal::LitString(bytes)))
                }
                x if x == LIT_TAG_ADDR => {
                    // Addr# is a legitimate intermediate runtime value: primops like
                    // PlusAddr / ShowDoubleAddr (see emit/primop.rs) emit
                    // SsaVal::Raw(_, LIT_TAG_ADDR), and any program that returns the
                    // raw address through the bridge surfaces here. We can't decode
                    // it back to a typed Haskell value (it's a raw pointer with no
                    // length), so we render an empty LitString as the safe fallback.
                    // See core-shapes.md §1.
                    Ok(Value::Lit(Literal::LitString(vec![])))
                }
                x if x == LIT_TAG_BYTEARRAY => {
                    // ByteArray# — raw pointer to [len: u64][bytes...]
                    let ba_ptr = raw_value as *const u8;
                    if ba_ptr.is_null() {
                        // ByteArray# legitimately can be empty/null in some Haskell programs;
                        // returning empty is correct. See docs/core-shapes/audit-heap-bridge.md#littagbytearray.
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
                        return Err(BridgeError::NullPointer);
                    }
                    let len = std::ptr::read_unaligned(arr_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let mut elems = Vec::with_capacity(len);
                    for i in 0..len {
                        let elem_ptr = *(arr_ptr.add(8 + 8 * i) as *const *const u8);
                        elems.push(heap_to_value_inner(elem_ptr, depth + 1, vmctx)?);
                    }
                    // SmallArray#/Array# carry no per-array DataConId. The wrapping Con (e.g.
                    // Vector's Array constructor) supplies type context to downstream consumers.
                    // DataConId(0) here is a deliberate sentinel meaning "raw boxed-pointer array".
                    // This is the contract documented in docs/core-shapes/audit-heap-bridge.md#littagarray--littagsmallarray.
                    Ok(Value::Con(DataConId(0), elems))
                }
                other => Err(BridgeError::UnexpectedLitTag(other as u8)),
            }
        }
        t if t == layout::TAG_CON => {
            let con_tag = unsafe { *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64) };
            let num_fields =
                unsafe { *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) }
                    as usize;
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
            // core-shapes.md §8: Closures are opaque and should not appear as top-level bridge results.
            // If we hit one, it indicates an unforced thunk leaked through or an invalid shape.
            Err(BridgeError::UnexpectedHeapTag(layout::TAG_CLOSURE))
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
    match val {
        Value::Lit(lit) => {
            let ptr = bump_alloc_from_vmctx(vmctx, layout::LIT_TOTAL_SIZE as usize);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);

            match lit {
                Literal::LitInt(n) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_INT as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *n;
                }
                Literal::LitWord(n) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_WORD as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *n;
                }
                Literal::LitChar(c) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_CHAR as u8;
                    // Ensure the full 8-byte slot is written to avoid reading UB junk later
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *c as u32 as u64;
                }
                Literal::LitFloat(bits) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_FLOAT as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *bits;
                }
                Literal::LitDouble(bits) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_DOUBLE as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *bits;
                }
                Literal::LitString(bytes) => {
                    // LitString stored as Lit with tag=5, value = ptr to [len: u64][bytes...]
                    // We allocate via the stable runtime allocator to avoid nursery movement.
                    let data_ptr =
                        crate::host_fns::runtime_new_byte_array(bytes.len() as i64) as *mut u8;
                    if data_ptr.is_null() {
                        return Err(BridgeError::NurseryExhausted);
                    }
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr.add(8), bytes.len());

                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_STRING as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = data_ptr as i64;
                }
            }
            Ok(ptr)
        }
        Value::Con(id, fields) => {
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
    use tidepool_repr::Literal;

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
    fn test_lit_float_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bits = 1.234f32.to_bits() as u64;
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
    fn test_lit_double_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bits = 5.678f64.to_bits();
        let val = Value::Lit(Literal::LitDouble(bits));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitDouble(b)) = back else {
                panic!("Expected LitDouble, got {:?}", back);
            };
            assert_eq!(b, bits);
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
    fn test_con_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(2048);
        let val = Value::Con(
            DataConId(42),
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::Lit(Literal::LitInt(2)),
            ],
        );
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Con(id, fields) = back else {
                panic!("Expected Con, got {:?}", back);
            };
            assert_eq!(id.0, 42);
            assert_eq!(fields.len(), 2);
            assert!(matches!(fields[0], Value::Lit(Literal::LitInt(1))));
            assert!(matches!(fields[1], Value::Lit(Literal::LitInt(2))));
        }
    }

    #[test]
    fn test_invalid_heap_tag() {
        let mut nursery = Nursery::new(1024);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        unsafe {
            let ptr = bump_alloc_from_vmctx(&mut vmctx, 8);
            *ptr = 0xFE; // Invalid tag
            let res = heap_to_value(ptr);
            assert!(matches!(res, Err(BridgeError::UnexpectedHeapTag(0xFE))));
        }
    }

    #[test]
    fn test_tag_closure_error() {
        let mut nursery = Nursery::new(1024);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        unsafe {
            let ptr = bump_alloc_from_vmctx(&mut vmctx, 8);
            *ptr = layout::TAG_CLOSURE;
            let res = heap_to_value(ptr);
            assert!(matches!(
                res,
                Err(BridgeError::UnexpectedHeapTag(layout::TAG_CLOSURE))
            ));
        }
    }
}
