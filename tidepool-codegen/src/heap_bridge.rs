use crate::context::VMContext;
use tidepool_eval::value::Value;
use tidepool_heap::layout;
use tidepool_repr::{DataConId, Literal};
use std::fmt;

#[derive(Debug)]
pub enum BridgeError {
    UnexpectedHeapTag(u8),
    UnexpectedLitTag(u8),
    NullPointer,
    NurseryExhausted,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BridgeError::UnexpectedHeapTag(t) => write!(f, "unexpected heap tag: {}", t),
            BridgeError::UnexpectedLitTag(t) => write!(f, "unexpected lit tag: {}", t),
            BridgeError::NullPointer => write!(f, "null pointer"),
            BridgeError::NurseryExhausted => write!(f, "nursery exhausted"),
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
    if ptr.is_null() {
        return Err(BridgeError::NullPointer);
    }

    let tag = *ptr;
    match tag {
        t if t == layout::TAG_LIT => {
            let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET) as i64;
            let raw_value = *(ptr.add(layout::LIT_VALUE_OFFSET) as *const i64);

            match lit_tag {
                0 => Ok(Value::Lit(Literal::LitInt(raw_value))),
                1 => Ok(Value::Lit(Literal::LitWord(raw_value as u64))),
                2 => Ok(Value::Lit(Literal::LitChar(
                    char::from_u32(raw_value as u32).unwrap_or('\0'),
                ))),
                3 => Ok(Value::Lit(Literal::LitFloat(raw_value as u64))),
                4 => Ok(Value::Lit(Literal::LitDouble(raw_value as u64))),
                5 => {
                    // LitString: value is pointer to [len: u64][bytes...]
                    // Use read_unaligned because JIT data sections may not be 8-byte aligned
                    let data_ptr = raw_value as *const u8;
                    if data_ptr.is_null() {
                        return Err(BridgeError::NullPointer);
                    }
                    let len = std::ptr::read_unaligned(data_ptr as *const u64) as usize;
                    let bytes_ptr = data_ptr.add(8);
                    let bytes = std::slice::from_raw_parts(bytes_ptr, len).to_vec();
                    Ok(Value::Lit(Literal::LitString(bytes)))
                }
                6 => {
                    // Addr# — intermediate value, shouldn't normally be a final result.
                    // Wrap as empty LitString as graceful fallback.
                    Ok(Value::Lit(Literal::LitString(vec![])))
                }
                other => Err(BridgeError::UnexpectedLitTag(other as u8)),
            }
        }
        t if t == layout::TAG_CON => {
            let con_tag = *(ptr.add(layout::CON_TAG_OFFSET) as *const u64);
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16) as usize;
            let mut fields = Vec::with_capacity(num_fields);
            for i in 0..num_fields {
                let field_ptr =
                    *(ptr.add(layout::CON_FIELDS_OFFSET + 8 * i) as *const *const u8);
                fields.push(heap_to_value(field_ptr)?);
            }
            Ok(Value::Con(DataConId(con_tag), fields))
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
    match val {
        Value::Lit(lit) => {
            let ptr = bump_alloc_from_vmctx(vmctx, layout::LIT_SIZE);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }

            match lit {
                Literal::LitInt(n) => {
                    layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET) = 0;
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = *n;
                }
                Literal::LitWord(n) => {
                    layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET) = 1;
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = *n as i64;
                }
                Literal::LitChar(c) => {
                    layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET) = 2;
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = *c as i64;
                }
                Literal::LitFloat(bits) => {
                    layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET) = 3;
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = *bits as i64;
                }
                Literal::LitDouble(bits) => {
                    layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET) = 4;
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = *bits as i64;
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
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        data_ptr.add(8),
                        bytes.len(),
                    );
                    
                    // Only write the header once we're sure all allocations succeeded
                    layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
                    *ptr.add(layout::LIT_TAG_OFFSET) = 5;
                    *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = data_ptr as i64;
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
            layout::write_header(ptr, layout::TAG_CON, size as u16);

            *(ptr.add(layout::CON_TAG_OFFSET) as *mut u64) = id.0;
            *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = fields.len() as u16;

            for (i, fp) in field_ptrs.into_iter().enumerate() {
                *(ptr.add(layout::CON_FIELDS_OFFSET + 8 * i) as *mut *mut u8) = fp;
            }
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
