//! Primop runtime implementations not inlined as Cranelift IR: byte-array and
//! boxed-array (`ByteArray#`/`Array#`) ops, Double decode/show/libm math, text
//! measurement helpers, and the pure JSON-decode (`eitherDecodeValue`) primop.

use crate::context::VMContext;
use crate::machine_state::{current_machine, machine_state, ExternalStorageKind};

use super::errors::{
    check_ptr_invalid, error_poison_ptr, overwrite_runtime_error, runtime_error_with_msg,
    runtime_oom, RuntimeError, MIN_VALID_ADDR,
};
use super::force::heap_force;
use super::gc::write_barrier;

// ---------------------------------------------------------------------------
// ByteArray runtime functions
// ---------------------------------------------------------------------------

/// Allocate a new mutable byte array of `size` bytes, zeroed.
/// Layout: [u64 length][u8 bytes...]
/// Returns a raw pointer to the allocation (caller stores in Lit value slot).
/// Mutable byte arrays are malloc'd with a hidden capacity word BELOW the
/// returned pointer:
///
/// ```text
///   base: [u64 total alloc size][u64 logical len][data ...]
///                                ^returned ba     ^ba + 8
/// ```
///
/// The JIT ABI (logical length at `ba`, data at `ba + 8`) is unchanged.
/// `runtime_shrink_byte_array` rewrites only the LOGICAL length, so the
/// capacity word is the only sound source for the dealloc `Layout` in
/// `runtime_resize_byte_array` — deriving it from the (possibly shrunk)
/// logical prefix deallocated with the wrong layout, which is UB.
/// (proptest_host_arrays BUG-2)
const BYTE_ARRAY_BASE_OFFSET: usize = 8;

fn external_allocation_failure(kind: ExternalStorageKind, bytes: usize) -> i64 {
    overwrite_runtime_error(RuntimeError::ExternalAllocationFailed { kind, bytes });
    error_poison_ptr() as i64
}

/// Allocate and register a GC-external buffer before the caller initializes
/// or publishes any part of it. Host functions in this module lack `vmctx`,
/// so production reaches the owning machine through the run-scoped
/// `CURRENT_MACHINE`; the registry guard installs/restores that pointer around
/// exactly one machine's JIT entry. Direct low-level tests without an installed
/// machine retain their historical untracked allocation behavior.
fn allocate_external(
    total: usize,
    published_offset: usize,
    kind: ExternalStorageKind,
    logical_len: usize,
    zeroed: bool,
) -> Result<*mut u8, i64> {
    let layout = std::alloc::Layout::from_size_align(total, 8)
        .map_err(|_| external_allocation_failure(kind, total))?;
    // SAFETY: `layout` is valid. A null result is handled before any caller
    // initialization can run.
    let base = unsafe {
        if zeroed {
            std::alloc::alloc_zeroed(layout)
        } else {
            std::alloc::alloc(layout)
        }
    };
    if base.is_null() {
        return Err(external_allocation_failure(kind, total));
    }
    // SAFETY: published_offset is supplied only as 0 (boxed arrays) or 8 for
    // the byte layout, both within their nonempty allocations.
    let published = unsafe { base.add(published_offset) };
    if let Some(ms) = unsafe { current_machine() } {
        ms.register_external_storage(published, base, layout, kind, logical_len);
    }
    Ok(published)
}

pub extern "C" fn runtime_new_byte_array(size: i64) -> i64 {
    if size < 0 {
        overwrite_runtime_error(RuntimeError::UserErrorMsg(
            "negative size in byte array allocation".to_string(),
        ));
        return error_poison_ptr() as i64;
    }
    let Some(total) = (2 * BYTE_ARRAY_BASE_OFFSET).checked_add(size as usize) else {
        return external_allocation_failure(ExternalStorageKind::Bytes, usize::MAX);
    };
    let ba = match allocate_external(
        total,
        BYTE_ARRAY_BASE_OFFSET,
        ExternalStorageKind::Bytes,
        size as usize,
        true,
    ) {
        Ok(ptr) => ptr,
        Err(poison) => return poison,
    };
    // SAFETY: base is a valid fresh allocation; capacity word at offset 0,
    // logical length prefix at offset 8 (= the returned ba's offset 0).
    unsafe {
        let base = ba.sub(BYTE_ARRAY_BASE_OFFSET);
        *(base as *mut u64) = total as u64;
        *(ba as *mut u64) = size as u64;
        ba as i64
    }
}

/// Copy `len` bytes from `src` (Addr#) to `dest_ba` (ByteArray ptr) at `dest_off`.
pub extern "C" fn runtime_copy_addr_to_byte_array(src: i64, dest_ba: i64, dest_off: i64, len: i64) {
    if check_ptr_invalid(src as *const u8, "runtime_copy_addr_to_byte_array")
        || check_ptr_invalid(dest_ba as *const u8, "runtime_copy_addr_to_byte_array")
    {
        return;
    }
    if dest_off < 0 || len < 0 {
        return;
    }
    // SAFETY: dest_ba passed the null-guard above and points to a byte array
    // with a u64 length prefix at offset 0.
    let dest_size = unsafe { *(dest_ba as *const u64) } as usize;
    if (dest_off as usize).saturating_add(len as usize) > dest_size {
        return;
    }
    let src_ptr = src as *const u8;
    // SAFETY: dest_ba + 8 + dest_off is within the byte array (bounds checked above).
    let dest_ptr = unsafe { (dest_ba as *mut u8).add(8 + dest_off as usize) };
    // SAFETY: src is a valid Addr# from JIT code, dest is within bounds, and regions
    // do not overlap (src is external memory, dest is a byte array).
    unsafe {
        std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, len as usize);
    }
}

/// Set `len` bytes in `ba` starting at `off` to `val`.
pub extern "C" fn runtime_set_byte_array(ba: i64, off: i64, len: i64, val: i64) {
    if check_ptr_invalid(ba as *const u8, "runtime_set_byte_array") {
        return;
    }
    if off < 0 || len < 0 {
        return;
    }
    let ba_size = unsafe { *(ba as *const u64) } as usize;
    if (off as usize).saturating_add(len as usize) > ba_size {
        return;
    }
    // SAFETY: ba passed the null-guard above; offsetting past the 8-byte length prefix + off.
    let ptr = unsafe { (ba as *mut u8).add(8 + off as usize) };
    // SAFETY: ptr is within the byte array allocation.
    unsafe {
        std::ptr::write_bytes(ptr, val as u8, len as usize);
    }
}

/// Shrink a mutable byte array to `new_size` bytes (just updates the length prefix).
pub extern "C" fn runtime_shrink_byte_array(ba: i64, new_size: i64) {
    if new_size < 0 || (ba as u64) < MIN_VALID_ADDR {
        return;
    }
    let old_size = unsafe { *(ba as *const u64) } as i64;
    if new_size > old_size {
        return; // only allow shrink, not grow
    }
    // SAFETY: ba is a valid byte array pointer from JIT code. Writing the length
    // prefix at offset 0 with a smaller value (logical shrink, no reallocation).
    unsafe {
        *(ba as *mut u64) = new_size as u64;
    }
    if let Some(ms) = unsafe { current_machine() } {
        ms.set_external_logical_len(ba as *mut u8, new_size as usize);
    }
}

/// Resize a mutable byte array. Allocates a new buffer, copies existing data,
/// zeroes any new bytes, and frees the old buffer. Returns the new pointer.
pub extern "C" fn runtime_resize_byte_array(ba: i64, new_size: i64) -> i64 {
    if new_size < 0 {
        overwrite_runtime_error(RuntimeError::UserErrorMsg(
            "negative size in byte array allocation".to_string(),
        ));
        return error_poison_ptr() as i64;
    }
    if (ba as u64) < MIN_VALID_ADDR {
        return error_poison_ptr() as i64;
    }
    let old_ptr = ba as *mut u8;
    // SAFETY: old_ptr passed the validity check above; logical length prefix at
    // offset 0, hidden capacity word at offset -8 (see runtime_new_byte_array).
    let old_size = unsafe { *(old_ptr as *const u64) } as usize;
    let old_base = unsafe { old_ptr.sub(BYTE_ARRAY_BASE_OFFSET) };
    // The TRUE allocation size — independent of any logical shrink since
    // allocation. Deriving the dealloc layout from the logical prefix after a
    // shrink(M) deallocated with size 8+M instead of the allocated size: UB.
    // (proptest_host_arrays BUG-2)
    let old_total = unsafe { *(old_base as *const u64) } as usize;
    let new_size = new_size as usize;

    let Some(new_total) = (2 * BYTE_ARRAY_BASE_OFFSET).checked_add(new_size) else {
        return external_allocation_failure(ExternalStorageKind::Bytes, usize::MAX);
    };
    let new_ptr = match allocate_external(
        new_total,
        BYTE_ARRAY_BASE_OFFSET,
        ExternalStorageKind::Bytes,
        new_size,
        true,
    ) {
        Ok(ptr) => ptr,
        Err(poison) => return poison,
    };
    let new_base = unsafe { new_ptr.sub(BYTE_ARRAY_BASE_OFFSET) };

    // Copy existing data (up to min of old/new logical size)
    let copy_len = old_size.min(new_size);
    // SAFETY: Both old and new buffers have data starting at offset 8 past the
    // logical prefix. copy_len <= min(old_size, new_size) so reads/writes are in
    // bounds (logical size never exceeds backing capacity).
    unsafe {
        std::ptr::copy_nonoverlapping(old_ptr.add(8), new_ptr.add(8), copy_len);
    }

    // SAFETY: fresh allocation; capacity word at base, logical prefix at ba.
    unsafe {
        *(new_base as *mut u64) = new_total as u64;
        *(new_ptr as *mut u64) = new_size as u64;
    }

    // Free old buffer with its RECORDED allocation layout.
    if let Some(ms) = unsafe { current_machine() } {
        if !ms.release_external_storage(old_ptr) {
            let old_layout = match std::alloc::Layout::from_size_align(old_total, 8) {
                Ok(layout) => layout,
                Err(_) => {
                    return external_allocation_failure(ExternalStorageKind::Bytes, old_total);
                }
            };
            unsafe { std::alloc::dealloc(old_base, old_layout) };
        }
    } else {
        let old_layout = match std::alloc::Layout::from_size_align(old_total, 8) {
            Ok(layout) => layout,
            Err(_) => return external_allocation_failure(ExternalStorageKind::Bytes, old_total),
        };
        unsafe { std::alloc::dealloc(old_base, old_layout) };
    }

    new_ptr as i64
}

/// Copy `len` bytes between byte arrays: src[src_off..] -> dest[dest_off..].
/// Used by both copyByteArray# and copyMutableByteArray#.
pub extern "C" fn runtime_copy_byte_array(
    src: i64,
    src_off: i64,
    dest: i64,
    dest_off: i64,
    len: i64,
) {
    if check_ptr_invalid(src as *const u8, "runtime_copy_byte_array")
        || check_ptr_invalid(dest as *const u8, "runtime_copy_byte_array")
    {
        return;
    }
    // Before the pointer arithmetic, validate offsets
    let src_size = unsafe { *(src as *const u64) } as usize;
    let dest_size = unsafe { *(dest as *const u64) } as usize;
    if src_off < 0 || dest_off < 0 || len < 0 {
        return; // silently return for negative offsets (matches GHC behavior)
    }
    let src_off = src_off as usize;
    let dest_off = dest_off as usize;
    let len = len as usize;
    if src_off.saturating_add(len) > src_size || dest_off.saturating_add(len) > dest_size {
        return; // out of bounds
    }

    // SAFETY: src and dest passed the null-guard above. Offsetting past the 8-byte
    // length prefix + the respective offsets.
    let src_ptr = unsafe { (src as *const u8).add(8 + src_off) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + dest_off) };
    // SAFETY: Uses copy (not copy_nonoverlapping) because src and dest may be the
    // same array with overlapping ranges.
    unsafe {
        std::ptr::copy(src_ptr, dest_ptr, len);
    }
}

/// Compare byte arrays: returns -1, 0, or 1.
pub extern "C" fn runtime_compare_byte_arrays(
    a: i64,
    a_off: i64,
    b: i64,
    b_off: i64,
    len: i64,
) -> i64 {
    if check_ptr_invalid(a as *const u8, "runtime_compare_byte_arrays")
        || check_ptr_invalid(b as *const u8, "runtime_compare_byte_arrays")
    {
        return 0;
    }
    if a_off < 0 || b_off < 0 || len < 0 {
        return 0;
    }
    let a_size = unsafe { *(a as *const u64) } as usize;
    let b_size = unsafe { *(b as *const u64) } as usize;
    if (a_off as usize).saturating_add(len as usize) > a_size
        || (b_off as usize).saturating_add(len as usize) > b_size
    {
        return 0;
    }

    // SAFETY: a and b passed the null-guard above. Offsetting past the 8-byte length
    // prefix + the respective offsets.
    let a_ptr = unsafe { (a as *const u8).add(8 + a_off as usize) };
    let b_ptr = unsafe { (b as *const u8).add(8 + b_off as usize) };
    let a_slice = unsafe { std::slice::from_raw_parts(a_ptr, len as usize) };
    let b_slice = unsafe { std::slice::from_raw_parts(b_ptr, len as usize) };
    match a_slice.cmp(b_slice) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

// ---------------------------------------------------------------------------
// Boxed array runtime functions (SmallArray# / Array#)
// ---------------------------------------------------------------------------

/// Allocate a new boxed array of `len` pointer slots, each initialized to `init`.
/// Layout: `[u64 length][ptr0][ptr1]...[ptrN-1]`
/// Each slot is 8 bytes (a heap pointer).
pub extern "C" fn runtime_new_boxed_array(len: i64, init: i64) -> i64 {
    if len < 0 {
        overwrite_runtime_error(RuntimeError::UserErrorMsg(
            "negative length in array allocation".to_string(),
        ));
        return error_poison_ptr() as i64;
    }
    let n = len as usize;
    let slot_bytes = match n.checked_mul(8) {
        Some(v) => v,
        None => {
            overwrite_runtime_error(RuntimeError::UserErrorMsg(
                "array size overflow".to_string(),
            ));
            return error_poison_ptr() as i64;
        }
    };
    let total = match 8usize.checked_add(slot_bytes) {
        Some(v) => v,
        None => {
            overwrite_runtime_error(RuntimeError::UserErrorMsg(
                "array size overflow".to_string(),
            ));
            return error_poison_ptr() as i64;
        }
    };
    let ptr = match allocate_external(total, 0, ExternalStorageKind::BoxedArray, n, false) {
        Ok(ptr) => ptr,
        Err(poison) => return poison,
    };
    // SAFETY: ptr is a fresh allocation of (8 + 8*n) bytes. Initializing all
    // pointer slots to `init` and then writing the length prefix.
    unsafe {
        let slots = ptr.add(8) as *mut i64;
        for i in 0..n {
            *slots.add(i) = init;
        }
        // Write length after slots are initialized so a concurrent reader
        // (e.g. GC walking) never sees a length prefix with uninit slots.
        *(ptr as *mut u64) = n as u64;
    }
    ptr as i64
}

/// Clone a sub-range of a boxed array: src[off..off+len].
pub extern "C" fn runtime_clone_boxed_array(src: i64, off: i64, len: i64) -> i64 {
    if (src as u64) < MIN_VALID_ADDR {
        return error_poison_ptr() as i64;
    }
    if len < 0 {
        overwrite_runtime_error(RuntimeError::UserErrorMsg(
            "negative length in array allocation".to_string(),
        ));
        return error_poison_ptr() as i64;
    }
    let n = len as usize;
    let slot_bytes = match n.checked_mul(8) {
        Some(v) => v,
        None => {
            overwrite_runtime_error(RuntimeError::UserErrorMsg(
                "array size overflow".to_string(),
            ));
            return error_poison_ptr() as i64;
        }
    };
    let total = match 8usize.checked_add(slot_bytes) {
        Some(v) => v,
        None => {
            overwrite_runtime_error(RuntimeError::UserErrorMsg(
                "array size overflow".to_string(),
            ));
            return error_poison_ptr() as i64;
        }
    };

    // Before the pointer arithmetic, validate offsets against source
    let src_n = unsafe { *(src as *const u64) } as usize;
    if off < 0 || (off as usize).saturating_add(n) > src_n {
        return error_poison_ptr() as i64; // silently return
    }

    let ptr = match allocate_external(total, 0, ExternalStorageKind::BoxedArray, n, false) {
        Ok(ptr) => ptr,
        Err(poison) => return poison,
    };
    // SAFETY: ptr is a fresh allocation. src is a valid boxed array from JIT code.
    // Copying len pointer slots from src[off..off+len] to the new array.
    unsafe {
        let src_slots = (src as *const u8).add(8 + 8 * off as usize);
        let dst_slots = ptr.add(8);
        std::ptr::copy_nonoverlapping(src_slots, dst_slots, 8 * n);
        // Publish the readable length only after every reference slot exists.
        *(ptr as *mut u64) = n as u64;
    }
    ptr as i64
}

/// Copy `len` pointer slots from src[src_off..] to dest[dest_off..]. `vmctx`
/// routes every dest slot through `write_barrier` (the dest range is only
/// known post-bounds-validation, unlike `WriteSmallArray`'s single
/// emit-time-computed slot) — the SAME barrier API every other old-to-young
/// array store uses, not a parallel mechanism.
pub extern "C" fn runtime_copy_boxed_array(
    vmctx: *mut VMContext,
    src: i64,
    src_off: i64,
    dest: i64,
    dest_off: i64,
    len: i64,
) {
    if (src as u64) < MIN_VALID_ADDR || (dest as u64) < MIN_VALID_ADDR {
        return;
    }
    if src_off < 0 || dest_off < 0 || len < 0 {
        return;
    }
    let src_n = unsafe { *(src as *const u64) } as usize;
    let dest_n = unsafe { *(dest as *const u64) } as usize;
    let src_off = src_off as usize;
    let dest_off = dest_off as usize;
    let len = len as usize;
    if src_off.saturating_add(len) > src_n || dest_off.saturating_add(len) > dest_n {
        return; // out of bounds
    }

    // SAFETY: src and dest are valid boxed array pointers from JIT code. Offsetting
    // past the 8-byte length prefix by the slot-sized offsets. Uses copy (not
    // copy_nonoverlapping) because src and dest may be the same array.
    let src_ptr = unsafe { (src as *const u8).add(8 + 8 * src_off) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + 8 * dest_off) };
    unsafe {
        std::ptr::copy(src_ptr, dest_ptr, 8 * len);
    }
    for i in 0..len {
        // SAFETY: dest_ptr..+8*len was just validated in-bounds and written above.
        let slot = unsafe { (dest_ptr as *mut *mut u8).add(i) };
        write_barrier(vmctx, slot);
    }
}

/// Shrink a boxed array (just update the length field).
pub extern "C" fn runtime_shrink_boxed_array(arr: i64, new_len: i64) {
    if new_len < 0 || (arr as u64) < MIN_VALID_ADDR {
        return;
    }
    let old_len = unsafe { *(arr as *const u64) } as i64;
    if new_len > old_len {
        return; // only allow shrink, not grow
    }
    // SAFETY: arr is a valid boxed array pointer from JIT code. Writing the length
    // prefix at offset 0 with a smaller value (logical shrink).
    unsafe {
        *(arr as *mut u64) = new_len as u64;
    }
    if let Some(ms) = unsafe { current_machine() } {
        ms.set_external_logical_len(arr as *mut u8, new_len as usize);
    }
}

/// CAS on a boxed array slot: compare-and-swap `arr[idx]`.
/// Returns the old value. If old == expected, writes new. `vmctx` routes a
/// successful write through `write_barrier` — the slot address is only known
/// post-bounds-validation, same reasoning as `runtime_copy_boxed_array`.
pub extern "C" fn runtime_cas_boxed_array(
    vmctx: *mut VMContext,
    arr: i64,
    idx: i64,
    expected: i64,
    new: i64,
) -> i64 {
    if (arr as u64) < MIN_VALID_ADDR || idx < 0 {
        return error_poison_ptr() as i64;
    }
    let n = unsafe { *(arr as *const u64) } as usize;
    if idx as usize >= n {
        return error_poison_ptr() as i64;
    }
    // SAFETY: arr is a valid boxed array pointer from JIT code. idx is within bounds.
    // Reading and conditionally writing a single pointer-sized slot.
    let slot = unsafe { (arr as *mut u8).add(8 + 8 * idx as usize) as *mut i64 };
    let old = unsafe { *slot };
    if old == expected {
        unsafe { *slot = new };
        write_barrier(vmctx, slot as *mut *mut u8);
    }
    old
}

/// Decode a Double into its Int64 mantissa (significand).
/// GHC's `decodeDouble_Int64#` returns (# mantissa, exponent #).
pub extern "C" fn runtime_decode_double_mantissa(bits: i64) -> i64 {
    let (man, _) = tidepool_bignum::decode_double_int64(f64::from_bits(bits as u64));
    man
}

/// Decode a Double into its Int exponent.
pub extern "C" fn runtime_decode_double_exponent(bits: i64) -> i64 {
    let (_, exp) = tidepool_bignum::decode_double_int64(f64::from_bits(bits as u64));
    exp
}

/// Decode a Float into its Int mantissa (significand).
/// GHC's `decodeFloat_Int#` returns (# mantissa, exponent #).
pub extern "C" fn runtime_decode_float_mantissa(bits: i64) -> i64 {
    let (man, _) = tidepool_bignum::decode_float_int(f32::from_bits(bits as u32));
    man
}

/// Decode a Float into its Int exponent.
pub extern "C" fn runtime_decode_float_exponent(bits: i64) -> i64 {
    let (_, exp) = tidepool_bignum::decode_float_int(f32::from_bits(bits as u32));
    exp
}

/// strlen: count bytes until null terminator.
pub extern "C" fn runtime_strlen(addr: i64) -> i64 {
    if check_ptr_invalid(addr as *const u8, "runtime_strlen") {
        return 0;
    }
    let ptr = addr as *const u8;
    let mut len = 0i64;
    // SAFETY: addr passed the null-guard above. The pointer is a null-terminated
    // C string from JIT data sections or unpackCString#. Scanning until '\0'.
    unsafe {
        while *ptr.add(len as usize) != 0 {
            len += 1;
        }
    }
    len
}

/// Measure codepoints in a UTF-8 buffer. Matches text-2.1.2 `_hs_text_measure_off` semantics.
///
/// If the buffer contains >= `cnt` characters, returns the non-negative byte count
/// of those `cnt` characters. If the buffer is shorter (< `cnt` chars), returns
/// the non-positive negated total character count. Returns 0 if `len` = 0 or `cnt` = 0.
///
/// # Safety
/// Input must be valid UTF-8. No validation is performed (matches C text library).
pub extern "C" fn runtime_text_measure_off(addr: i64, off: i64, len: i64, cnt: i64) -> i64 {
    if len <= 0 || cnt <= 0 {
        return 0;
    }
    if check_ptr_invalid(addr as *const u8, "runtime_text_measure_off") {
        return 0;
    }
    let ptr = (addr + off) as *const u8;
    let len = len as usize;
    let cnt = cnt as usize;
    let mut byte_pos = 0usize;
    let mut chars_found = 0usize;
    while chars_found < cnt && byte_pos < len {
        // SAFETY: byte_pos < len, so ptr + byte_pos is within the buffer.
        let b = unsafe { *ptr.add(byte_pos) };
        let char_len = if b < 0x80 {
            1
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        byte_pos += char_len;
        chars_found += 1;
    }
    if chars_found >= cnt {
        // Buffer had enough characters — return bytes consumed (non-negative)
        byte_pos as i64
    } else {
        // Buffer exhausted before cnt — return negated char count (non-positive)
        -(chars_found as i64)
    }
}

/// Find a byte in a buffer. Returns offset from start, or -1 if not found.
pub extern "C" fn runtime_text_memchr(addr: i64, off: i64, len: i64, needle: i64) -> i64 {
    if len <= 0 {
        return -1;
    }
    if check_ptr_invalid(addr as *const u8, "runtime_text_memchr") {
        return -1;
    }
    let ptr = (addr + off) as *const u8;
    // SAFETY: addr passed the null-guard above. ptr = addr + off points into a valid
    // Text buffer, and len bytes are readable from that position.
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    match slice.iter().position(|&b| b == needle as u8) {
        Some(pos) => pos as i64,
        None => -1,
    }
}

/// Reverse UTF-8 text. Matches text-2.1.2 `_hs_text_reverse(dst0, src0, off, len)`.
///
/// Reads `len` bytes from `src + off`, writes reversed codepoints starting at `dst`.
pub extern "C" fn runtime_text_reverse(dest: i64, src: i64, off: i64, len: i64) {
    if len <= 0 {
        return;
    }
    if check_ptr_invalid(dest as *const u8, "runtime_text_reverse")
        || check_ptr_invalid(src as *const u8, "runtime_text_reverse")
    {
        return;
    }
    let src_ptr = (src + off) as *const u8;
    // SAFETY: src + off points into a valid Text buffer and len bytes are readable.
    let src_slice = unsafe { std::slice::from_raw_parts(src_ptr, len as usize) };
    let dest_ptr = dest as *mut u8;
    // Decode UTF-8 codepoints, write in reverse order
    let mut read_pos = 0usize;
    let mut write_pos = len as usize;
    while read_pos < len as usize {
        let b = src_slice[read_pos];
        let char_len = if b < 0x80 {
            1
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        write_pos -= char_len;
        // SAFETY: read_pos and write_pos are within their respective buffers.
        // src and dest do not overlap (separate allocations from JIT code).
        unsafe {
            std::ptr::copy_nonoverlapping(
                src_slice.as_ptr().add(read_pos),
                dest_ptr.add(write_pos),
                char_len,
            );
        }
        read_pos += char_len;
    }
}

/// `quotRemWord2#` quotient: `((hi << 64) | lo) / d`. The native ghc-bignum
/// backend's 128/64 division primitive. `d == 0` is guarded by the Haskell
/// caller (raiseDivZero#); we still return 0 rather than panic.
pub extern "C" fn runtime_word2_quot(hi: i64, lo: i64, d: i64) -> i64 {
    let d = d as u64;
    if d == 0 {
        return 0;
    }
    let n = ((hi as u64 as u128) << 64) | (lo as u64 as u128);
    (n / d as u128) as u64 as i64
}

/// `quotRemWord2#` remainder: `((hi << 64) | lo) % d`.
pub extern "C" fn runtime_word2_rem(hi: i64, lo: i64, d: i64) -> i64 {
    let d = d as u64;
    if d == 0 {
        return 0;
    }
    let n = ((hi as u64 as u128) << 64) | (lo as u64 as u128);
    (n % d as u128) as u64 as i64
}

/// `__int_encodeDouble(mantissa, exp) -> Double#` (returned as raw f64 bits).
pub extern "C" fn runtime_int_encode_double(mantissa: i64, exp: i64) -> i64 {
    tidepool_bignum::encode_double(mantissa, exp).to_bits() as i64
}

/// `__word_encodeDouble(mantissa, exp) -> Double#` (unsigned mantissa; raw bits).
pub extern "C" fn runtime_word_encode_double(mantissa: i64, exp: i64) -> i64 {
    tidepool_bignum::encode_double_word(mantissa as u64, exp).to_bits() as i64
}

/// Format a Double and allocate the result as a managed Haskell `Text` value.
unsafe fn render_double_text(vmctx: *mut VMContext, prec: Option<i64>, bits: i64) -> *mut u8 {
    let text_id = match machine_state(vmctx).text_con_id() {
        Some(id) => id,
        None => {
            let msg = b"Double rendering: Text constructor not in scope";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };
    let d = f64::from_bits(bits as u64);
    let body = tidepool_bignum::haskell_show_double(d);
    let rendered = if prec.is_some_and(|p| p > 6) && d < 0.0 {
        format!("({body})")
    } else {
        body
    };
    let value = tidepool_bridge::shapes::make_text(&rendered, text_id);
    let converted = crate::heap_bridge::gc_retry(
        vmctx,
        |r: &Result<*mut u8, crate::heap_bridge::BridgeError>| {
            matches!(r, Err(crate::heap_bridge::BridgeError::NurseryExhausted))
        },
        || crate::heap_bridge::value_to_heap(&value, &mut *vmctx),
    );
    match converted {
        Ok(ptr) => ptr,
        Err(crate::heap_bridge::BridgeError::NurseryExhausted) => runtime_oom(),
        Err(e) => {
            let msg = format!("Double rendering failed: {e}");
            runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64)
        }
    }
}

/// Render a Double directly into a GC-owned `Text` value.
///
/// # Safety
///
/// `vmctx` must point to the live VM context for the calling JIT machine. Its
/// machine state, constructor registry, nursery, and stack maps must remain
/// installed for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn runtime_render_double_text(vmctx: *mut VMContext, bits: i64) -> *mut u8 {
    render_double_text(vmctx, None, bits)
}

/// Precedence-aware sibling of [`runtime_render_double_text`].
///
/// # Safety
///
/// The same contract as [`runtime_render_double_text`] applies.
#[no_mangle]
pub unsafe extern "C" fn runtime_render_double_prec_text(
    vmctx: *mut VMContext,
    prec: i64,
    bits: i64,
) -> *mut u8 {
    render_double_text(vmctx, Some(prec), bits)
}

// --- Double math runtime functions (libm wrappers) ---
// All take f64-as-i64-bits and return f64-as-i64-bits.
macro_rules! double_math_unary {
    ($name:ident, $op:ident) => {
        pub extern "C" fn $name(bits: i64) -> i64 {
            let d = f64::from_bits(bits as u64);
            f64::$op(d).to_bits() as i64
        }
    };
}

double_math_unary!(runtime_double_exp, exp);
double_math_unary!(runtime_double_expm1, exp_m1);
double_math_unary!(runtime_double_log, ln);
double_math_unary!(runtime_double_log1p, ln_1p);
double_math_unary!(runtime_double_sin, sin);
double_math_unary!(runtime_double_cos, cos);
double_math_unary!(runtime_double_tan, tan);
double_math_unary!(runtime_double_asin, asin);
double_math_unary!(runtime_double_acos, acos);
double_math_unary!(runtime_double_atan, atan);
double_math_unary!(runtime_double_sinh, sinh);
double_math_unary!(runtime_double_cosh, cosh);
double_math_unary!(runtime_double_tanh, tanh);
double_math_unary!(runtime_double_asinh, asinh);
double_math_unary!(runtime_double_acosh, acosh);
double_math_unary!(runtime_double_atanh, atanh);

pub extern "C" fn runtime_double_power(bits_a: i64, bits_b: i64) -> i64 {
    let a = f64::from_bits(bits_a as u64);
    let b = f64::from_bits(bits_b as u64);
    a.powf(b).to_bits() as i64
}

/// `eitherDecodeValue :: Text -> Either Text Value` — the pure JSON-decode primop.
///
/// Lifts the argument `Text` to a `Value` tree via `heap_bridge` (which owns
/// the heap-byte layouts), slices its UTF-8 bytes via
/// `tidepool_bridge::shapes::text_bytes_clamped_with`, parses with
/// `serde_json`, and builds the aeson `Either Text Value` ADT on the nursery
/// heap via `tidepool_bridge::json_builder` + the stack-safe `value_to_heap`.
/// Parse
/// failure yields `Left <serde error message>`.
///
/// # Safety
/// `vmctx` must be a valid live VMContext; `text_ptr` a valid heap pointer to a
/// `Text` value (or a thunk that forces to one).
#[no_mangle]
pub unsafe extern "C" fn runtime_json_decode(vmctx: *mut VMContext, text_ptr: *mut u8) -> *mut u8 {
    let ids = match machine_state(vmctx).json_con_ids() {
        Some(ids) => ids,
        None => {
            let msg = b"eitherDecode: aeson Value/Either/Map constructors not in scope";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    // Force the Text to WHNF and lift it to a Value tree (forcing lazy fields
    // during traversal).
    let text = heap_force(vmctx, text_ptr);
    if text.is_null() {
        let msg = b"eitherDecode: null Text argument";
        return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
    }
    let text_val = match crate::heap_bridge::heap_to_value_forcing(text, vmctx) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("eitherDecode: Text argument read failed: {e}");
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };
    // No `&DataConTable` reaches this host fn (only the cached `JsonConIds`),
    // so recognize `I#` by the concrete id already resolved into `ids`; a
    // lifted `ByteArray` wrapper con has no such id here and goes
    // unrecognized by the native JsonDecode path.
    let bytes = match &text_val {
        tidepool_bridge::Value::Con(_, fields) => tidepool_bridge::shapes::text_bytes_clamped_with(
            fields,
            |_| false,
            |id| id == ids.i_hash,
        ),
        _ => None,
    };
    let s = match bytes {
        Some(b) => String::from_utf8_lossy(&b).into_owned(),
        None => {
            let msg = b"eitherDecode: argument is not a Text (Con with 3 fields)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    // Build the shared material Value (GC-inert Rust data), then materialize on the heap
    // with one GC-and-retry (the deep spine converts stack-safely via the hylo
    // in value_to_heap).
    let value = match tidepool_bridge::json_builder::decode_json_str(&s, &ids) {
        Some(v) => v,
        None => {
            let msg = b"eitherDecode: Either (Left/Right) constructors not in scope";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };
    let converted = crate::heap_bridge::gc_retry(
        vmctx,
        |r: &Result<*mut u8, crate::heap_bridge::BridgeError>| {
            matches!(r, Err(crate::heap_bridge::BridgeError::NurseryExhausted))
        },
        || crate::heap_bridge::value_to_heap(&value, &mut *vmctx),
    );
    match converted {
        Ok(p) => p,
        Err(crate::heap_bridge::BridgeError::NurseryExhausted) => runtime_oom(),
        Err(e) => {
            let msg = format!("eitherDecode: result materialization failed: {e}");
            runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64)
        }
    }
}

/// JIT host fn for the `ParseISO8601` primop
/// (`parseISO8601 :: Text -> Either Text UTCTime`). Forces the `Text` argument,
/// extracts its UTF-8 bytes, parses via `chrono`, and builds the
/// `Either Text UTCTime` ADT on the nursery heap via
/// `tidepool_bridge::time::parse_iso8601_str` + the stack-safe
/// `value_to_heap`. A parse failure yields `Left <message>`.
///
/// # Safety
/// `vmctx` must be a valid live VMContext; `text_ptr` a valid heap pointer to a
/// `Text` value (or a thunk that forces to one).
#[no_mangle]
pub unsafe extern "C" fn runtime_parse_iso8601(
    vmctx: *mut VMContext,
    text_ptr: *mut u8,
) -> *mut u8 {
    let ids = match machine_state(vmctx).time_con_ids() {
        Some(ids) => ids,
        None => {
            let msg = b"parseISO8601: Either/I#/Text constructors not in scope";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    let text = heap_force(vmctx, text_ptr);
    if text.is_null() {
        let msg = b"parseISO8601: null Text argument";
        return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
    }
    let text_val = match crate::heap_bridge::heap_to_value_forcing(text, vmctx) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("parseISO8601: Text argument read failed: {e}");
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };
    let bytes = match &text_val {
        tidepool_bridge::Value::Con(_, fields) => tidepool_bridge::shapes::text_bytes_clamped_with(
            fields,
            |_| false,
            |id| id == ids.i_hash,
        ),
        _ => None,
    };
    let s = match bytes {
        Some(b) => String::from_utf8_lossy(&b).into_owned(),
        None => {
            let msg = b"parseISO8601: argument is not a Text (Con with 3 fields)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    let value = tidepool_bridge::time::parse_iso8601_str(&s, &ids);
    let converted = crate::heap_bridge::gc_retry(
        vmctx,
        |r: &Result<*mut u8, crate::heap_bridge::BridgeError>| {
            matches!(r, Err(crate::heap_bridge::BridgeError::NurseryExhausted))
        },
        || crate::heap_bridge::value_to_heap(&value, &mut *vmctx),
    );
    match converted {
        Ok(p) => p,
        Err(crate::heap_bridge::BridgeError::NurseryExhausted) => runtime_oom(),
        Err(e) => {
            let msg = format!("parseISO8601: result materialization failed: {e}");
            runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64)
        }
    }
}

#[cfg(test)]
#[allow(clippy::approx_constant)] // tests use 3.14 literal floats as round-trip data
mod tests {
    // SAFETY: All unsafe blocks in tests operate on allocations created within
    // the test via runtime_new_byte_array or stack-allocated buffers with known
    // sizes and layouts. Pointers and offsets are controlled by the test code.
    use super::*;
    use crate::machine_state::{install_current_machine, restore_current_machine, MachineState};

    struct CurrentMachineGuard(*mut MachineState);

    impl CurrentMachineGuard {
        fn install(machine: &MachineState) -> Self {
            Self(install_current_machine(
                machine as *const MachineState as *mut MachineState,
            ))
        }
    }

    impl Drop for CurrentMachineGuard {
        fn drop(&mut self) {
            restore_current_machine(self.0);
        }
    }

    #[test]
    fn external_allocations_are_owned_and_accounted() {
        let machine = MachineState::new();
        let _current = CurrentMachineGuard::install(&machine);

        let bytes = runtime_new_byte_array(13);
        let boxed = runtime_new_boxed_array(3, 0);
        assert_ne!(bytes as *mut u8, error_poison_ptr());
        assert_ne!(boxed as *mut u8, error_poison_ptr());

        let stats = machine.external_storage_stats();
        assert_eq!(stats.allocated_objects, 2);
        assert_eq!(stats.live_objects, 2);
        assert_eq!(stats.freed_objects, 0);
        assert_eq!(stats.allocated_bytes, 29 + 32);
        assert_eq!(stats.live_bytes, stats.allocated_bytes);
    }

    #[test]
    fn impossible_external_layout_returns_poison_and_typed_failure() {
        let machine = MachineState::new();
        let _current = CurrentMachineGuard::install(&machine);

        let result = runtime_new_byte_array(i64::MAX);
        assert_eq!(result as *mut u8, error_poison_ptr());
        assert_eq!(machine.external_storage_stats(), Default::default());
        assert!(matches!(
            machine.take_runtime_error(),
            Some(RuntimeError::ExternalAllocationFailed {
                kind: ExternalStorageKind::Bytes,
                ..
            })
        ));
    }

    #[test]
    fn concurrent_machine_external_ledgers_are_isolated() {
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [7_i64, 41_i64]
            .into_iter()
            .map(|size| {
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let machine = MachineState::new();
                    let _current = CurrentMachineGuard::install(&machine);
                    gate.wait();
                    let ptr = runtime_new_byte_array(size);
                    assert_ne!(ptr as *mut u8, error_poison_ptr());
                    gate.wait();
                    machine.external_storage_stats()
                })
            })
            .collect();
        let mut stats: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        stats.sort_by_key(|stats| stats.live_bytes);
        assert_eq!(stats[0].live_objects, 1);
        assert_eq!(stats[0].live_bytes, 23);
        assert_eq!(stats[1].live_objects, 1);
        assert_eq!(stats[1].live_bytes, 57);
    }

    #[test]
    fn test_runtime_strlen() {
        let s = b"hello\0world\0";
        unsafe {
            assert_eq!(runtime_strlen(s.as_ptr() as i64), 5);
            assert_eq!(runtime_strlen(s.as_ptr().add(6) as i64), 5);
        }
    }

    // ---------------------------------------------------------------
    // runtime_text_measure_off — text-2.1.2 semantics:
    //   cnt reached => return bytes consumed (non-negative)
    //   buffer exhausted => return -(chars_found) (non-positive)
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // runtime_text_reverse — text-2.1.2: reverse(dst, src, off, len)
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // runtime_text_memchr — memchr(arr, off, len, byte) -> offset or -1
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // decode_double_int64 — matches GHC's decodeDouble_Int64#
    // ---------------------------------------------------------------
}
