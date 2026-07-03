//! Primop runtime implementations not inlined as Cranelift IR: byte-array and
//! boxed-array (`ByteArray#`/`Array#`) ops, Double decode/show/libm math, text
//! measurement helpers, and the pure `decodeJson` primop.

use crate::context::VMContext;
use std::cell::Cell;

use super::errors::{
    check_ptr_invalid, error_poison_ptr, runtime_error_with_msg, runtime_oom, RuntimeError,
    MIN_VALID_ADDR, RUNTIME_ERROR,
};
use super::force::heap_force;
use super::gc::gc_trigger;

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

pub extern "C" fn runtime_new_byte_array(size: i64) -> i64 {
    if size < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative size in byte array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let total = (2 * BYTE_ARRAY_BASE_OFFSET).saturating_add(size as usize);
    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let base = unsafe { std::alloc::alloc_zeroed(layout) };
    if base.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: base is a valid fresh allocation; capacity word at offset 0,
    // logical length prefix at offset 8 (= the returned ba's offset 0).
    unsafe {
        *(base as *mut u64) = total as u64;
        let ba = base.add(BYTE_ARRAY_BASE_OFFSET);
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
}

/// Resize a mutable byte array. Allocates a new buffer, copies existing data,
/// zeroes any new bytes, and frees the old buffer. Returns the new pointer.
pub extern "C" fn runtime_resize_byte_array(ba: i64, new_size: i64) -> i64 {
    if new_size < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative size in byte array allocation".to_string(),
            ));
        });
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

    let new_total = (2 * BYTE_ARRAY_BASE_OFFSET).saturating_add(new_size);
    let new_layout =
        std::alloc::Layout::from_size_align(new_total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let new_base = unsafe { std::alloc::alloc_zeroed(new_layout) };
    if new_base.is_null() {
        std::alloc::handle_alloc_error(new_layout);
    }
    let new_ptr = unsafe { new_base.add(BYTE_ARRAY_BASE_OFFSET) };

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
    let old_layout =
        std::alloc::Layout::from_size_align(old_total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: old_base/old_total are exactly the pointer and layout produced by
    // the runtime_new/resize call that allocated this array.
    unsafe {
        std::alloc::dealloc(old_base, old_layout);
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
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative length in array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let n = len as usize;
    let slot_bytes = match n.checked_mul(8) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };
    let total = match 8usize.checked_add(slot_bytes) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };
    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc returns a valid allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
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
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative length in array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let n = len as usize;
    let slot_bytes = match n.checked_mul(8) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };
    let total = match 8usize.checked_add(slot_bytes) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };

    // Before the pointer arithmetic, validate offsets against source
    let src_n = unsafe { *(src as *const u64) } as usize;
    if off < 0 || (off as usize).saturating_add(n) > src_n {
        return error_poison_ptr() as i64; // silently return
    }

    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc returns a valid allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: ptr is a fresh allocation. src is a valid boxed array from JIT code.
    // Copying len pointer slots from src[off..off+len] to the new array.
    unsafe {
        *(ptr as *mut u64) = n as u64;
        let src_slots = (src as *const u8).add(8 + 8 * off as usize);
        let dst_slots = ptr.add(8);
        std::ptr::copy_nonoverlapping(src_slots, dst_slots, 8 * n);
    }
    ptr as i64
}

/// Copy `len` pointer slots from src[src_off..] to dest[dest_off..].
pub extern "C" fn runtime_copy_boxed_array(
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
}

/// CAS on a boxed array slot: compare-and-swap `arr[idx]`.
/// Returns the old value. If old == expected, writes new.
pub extern "C" fn runtime_cas_boxed_array(arr: i64, idx: i64, expected: i64, new: i64) -> i64 {
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
    }
    old
}

/// Decode a Double into its Int64 mantissa (significand).
/// GHC's `decodeDouble_Int64#` returns (# mantissa, exponent #).
pub extern "C" fn runtime_decode_double_mantissa(bits: i64) -> i64 {
    let (man, _) = decode_double_int64(f64::from_bits(bits as u64));
    man
}

/// Decode a Double into its Int exponent.
pub extern "C" fn runtime_decode_double_exponent(bits: i64) -> i64 {
    let (_, exp) = decode_double_int64(f64::from_bits(bits as u64));
    exp
}

/// Shared implementation matching GHC's `decodeDouble_Int64#` semantics.
/// Returns (mantissa, exponent) such that mantissa * 2^exponent == d,
/// with mantissa normalized to have no trailing zeros in binary.
fn decode_double_int64(d: f64) -> (i64, i64) {
    if d == 0.0 || d.is_nan() {
        return (0, 0);
    }
    if d.is_infinite() {
        return (if d > 0.0 { 1 } else { -1 }, 0);
    }
    let bits = d.to_bits();
    let sign: i64 = if bits >> 63 == 0 { 1 } else { -1 };
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let raw_man = (bits & 0x000f_ffff_ffff_ffff) as i64;
    let (man, exp) = if raw_exp == 0 {
        // subnormal
        (raw_man, 1 - 1023 - 52)
    } else {
        // normal: implicit leading 1
        (raw_man | (1i64 << 52), raw_exp - 1023 - 52)
    };
    let man = sign * man;
    if man != 0 {
        let tz = man.unsigned_abs().trailing_zeros();
        (man >> tz, (exp + tz as i32) as i64)
    } else {
        (0, 0)
    }
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

/// Format a Double as a null-terminated C string and return its address.
/// The CString is leaked (small bounded strings, acceptable).
pub extern "C" fn runtime_show_double_addr(bits: i64) -> i64 {
    let d = f64::from_bits(bits as u64);
    let s = haskell_show_double(d);
    let c_str = match std::ffi::CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::Undefined);
            });
            return error_poison_ptr() as i64;
        }
    };
    let ptr = c_str.into_raw();
    ptr as i64
}

/// Precedence-aware Double show — the `showSignedFloat` behavior the JIT-safe
/// replacement drops. Parenthesizes a NEGATIVE Double when `prec > 6` (the
/// constructor-argument precedence), matching GHC: `show (Just (-2.5))` is
/// `Just (-2.5)`, but top-level `show (-2.5)` (prec 0) is `-2.5`. The magnitude
/// string already carries the sign (`haskell_show_double`), so only the parens
/// are added here. Leaked CString, same contract as `runtime_show_double_addr`.
pub extern "C" fn runtime_show_signed_double_addr(prec: i64, bits: i64) -> i64 {
    let d = f64::from_bits(bits as u64);
    let body = haskell_show_double(d);
    // Match `showSignedFloat` EXACTLY: it parenthesizes on `x < 0` (not "renders
    // with a minus"). `-0.0 < 0` is False (IEEE), so GHC shows `Just -0.0`
    // WITHOUT parens; NaN (`NaN < 0` False) and +Inf never parenthesize; only
    // -Inf and negative normals (`< 0` True) do, at prec > 6.
    let s = if prec > 6 && d < 0.0 {
        format!("({body})")
    } else {
        body
    };
    let c_str = match std::ffi::CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::Undefined);
            });
            return error_poison_ptr() as i64;
        }
    };
    c_str.into_raw() as i64
}

/// Format a Double matching Haskell's `show` output.
/// Decimal notation for 0.1 <= |x| < 1e7, scientific notation otherwise.
/// Always includes a decimal point.
fn haskell_show_double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".to_string();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let abs = d.abs();
    if (0.1..1.0e7).contains(&abs) {
        let s = d.to_string();
        if s.contains('.') {
            s
        } else {
            format!("{}.0", s)
        }
    } else {
        // Scientific notation. Haskell's `show` mantissa always carries a
        // decimal point ("1.0e10", "5.0e-324"); Rust's {:e} omits it for
        // integral mantissas ("1e10"). Insert ".0" before the exponent when
        // missing. (proptest_host_arrays BUG-1)
        let s = format!("{:e}", d);
        match s.find('e') {
            Some(epos) if !s[..epos].contains('.') => {
                format!("{}.0{}", &s[..epos], &s[epos..])
            }
            _ => s,
        }
    }
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

thread_local! {
    /// The aeson-`Value` constructor ids for the currently-running JIT machine,
    /// installed by the run entry (`JitEffectMachine`) from the compile-time
    /// `DataConTable`. Read by [`runtime_json_decode`] to build the `Maybe
    /// Value` result. `None` when the aeson/Maybe/Map closure isn't in scope.
    static JSON_CON_IDS: Cell<Option<tidepool_eval::json::JsonConIds>> = const { Cell::new(None) };
}

/// Install the JSON constructor ids for this thread's JIT run. `None` clears.
pub fn set_json_con_ids(ids: Option<tidepool_eval::json::JsonConIds>) {
    JSON_CON_IDS.with(|c| c.set(ids));
}

/// `decodeJson :: Text -> Maybe Value` — the pure JSON-decode primop.
///
/// Lifts the argument `Text` to a `Value` tree via `heap_bridge` (which owns
/// the heap-byte layouts), slices its UTF-8 bytes via
/// `tidepool_eval::shapes::text_bytes_clamped_with` (the same table-free
/// Text decode the tree-walker's `JsonDecode` arm uses), parses with
/// `serde_json`, and builds the aeson `Maybe Value` ADT on the nursery heap
/// via `tidepool_eval::json` (the SAME builder the tree-walker uses, so JIT
/// and eval agree by construction) + the stack-safe `value_to_heap`. Parse
/// failure yields `Nothing`.
///
/// # Safety
/// `vmctx` must be a valid live VMContext; `text_ptr` a valid heap pointer to a
/// `Text` value (or a thunk that forces to one).
#[no_mangle]
pub unsafe extern "C" fn runtime_json_decode(vmctx: *mut VMContext, text_ptr: *mut u8) -> *mut u8 {
    let ids = match JSON_CON_IDS.with(|c| c.get()) {
        Some(ids) => ids,
        None => {
            let msg = b"decodeJson: aeson Value/Maybe/Map constructors not in scope";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    // Force the Text to WHNF and lift it to a Value tree (forcing lazy fields
    // during traversal).
    let text = heap_force(vmctx, text_ptr);
    if text.is_null() {
        let msg = b"decodeJson: null Text argument";
        return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
    }
    let text_val = match crate::heap_bridge::heap_to_value_forcing(text, vmctx) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("decodeJson: Text argument read failed: {e}");
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };
    // No `&DataConTable` reaches this host fn (only the cached `JsonConIds`),
    // so recognize `I#` by the concrete id already resolved into `ids`; a
    // lifted `ByteArray` wrapper con has no such id here and goes
    // unrecognized — exactly as in eval's `JsonDecode` arm.
    let bytes = match &text_val {
        tidepool_eval::Value::Con(_, fields) => tidepool_eval::shapes::text_bytes_clamped_with(
            fields,
            |_| false,
            |id| id == ids.i_hash,
        ),
        _ => None,
    };
    let s = match bytes {
        Some(b) => String::from_utf8_lossy(&b).into_owned(),
        None => {
            let msg = b"decodeJson: argument is not a Text (Con with 3 fields)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    // Build the eval Value (GC-inert Rust data), then materialize on the heap
    // with one GC-and-retry (the deep spine converts stack-safely via the hylo
    // in value_to_heap).
    let value = match tidepool_eval::json::decode_json_str(&s, &ids) {
        Some(v) => v,
        None => {
            let msg = b"decodeJson: Maybe (Just/Nothing) constructors not in scope";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };
    match crate::heap_bridge::value_to_heap(&value, &mut *vmctx) {
        Ok(p) => p,
        Err(crate::heap_bridge::BridgeError::NurseryExhausted) => {
            gc_trigger(vmctx);
            match crate::heap_bridge::value_to_heap(&value, &mut *vmctx) {
                Ok(p) => p,
                Err(_) => runtime_oom(),
            }
        }
        Err(e) => {
            let msg = format!("decodeJson: result materialization failed: {e}");
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
    use std::alloc::{dealloc, Layout};

    // SAFETY: ptr was allocated by runtime_new_byte_array with layout [8 + size, align 8].
    unsafe fn free_byte_array(ptr: i64) {
        // Mirror the capacity-word-below-pointer scheme: the TRUE allocation
        // size lives at ba - 8 and is immune to logical shrinks.
        let base = (ptr as *mut u8).sub(BYTE_ARRAY_BASE_OFFSET);
        let total = *(base as *const u64) as usize;
        let layout = Layout::from_size_align(total, 8).unwrap();
        dealloc(base, layout);
    }

    #[test]
    fn test_runtime_new_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            assert_ne!(ba, 0);
            assert_eq!(*(ba as *const u64), 10);
            let bytes = std::slice::from_raw_parts((ba as *const u8).add(8), 10);
            assert!(bytes.iter().all(|&b| b == 0));
            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_copy_addr_to_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            let src = b"hello";
            runtime_copy_addr_to_byte_array(src.as_ptr() as i64, ba, 2, 5);
            let bytes = std::slice::from_raw_parts((ba as *const u8).add(8), 10);
            assert_eq!(&bytes[2..7], b"hello");
            assert_eq!(bytes[0], 0);
            assert_eq!(bytes[1], 0);
            assert_eq!(bytes[7], 0);
            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_set_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            runtime_set_byte_array(ba, 3, 4, 0xFF);
            let bytes = std::slice::from_raw_parts((ba as *const u8).add(8), 10);
            assert_eq!(bytes[2], 0);
            assert_eq!(bytes[3], 0xFF);
            assert_eq!(bytes[4], 0xFF);
            assert_eq!(bytes[5], 0xFF);
            assert_eq!(bytes[6], 0xFF);
            assert_eq!(bytes[7], 0);
            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_shrink_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            runtime_shrink_byte_array(ba, 5);
            assert_eq!(*(ba as *const u64), 5);
            // Logical shrink only: the capacity word below the pointer still
            // records the original allocation, so free_byte_array (and
            // runtime_resize_byte_array) dealloc with the true layout.
            free_byte_array(ba);
        }
    }

    /// BUG-2 regression (proptest_host_arrays): shrink-then-resize must
    /// dealloc the old buffer with its TRUE allocation layout (capacity
    /// word), not one derived from the shrunken logical prefix.
    #[test]
    fn test_shrink_then_resize_uses_true_layout() {
        unsafe {
            let ba = runtime_new_byte_array(64);
            for i in 0..64u8 {
                runtime_set_byte_array(ba, i as i64, 1, i as i64);
            }
            runtime_shrink_byte_array(ba, 5);
            // Old code derived the dealloc layout from the logical prefix (5)
            // here — UB. With the capacity word this deallocs 16+64 correctly.
            let resized = runtime_resize_byte_array(ba, 128);
            assert_eq!(*(resized as *const u64), 128);
            // Logical content (first 5 bytes) preserved; grown tail zeroed.
            for i in 0..5u8 {
                assert_eq!(*((resized as *const u8).add(8 + i as usize)), i);
            }
            assert_eq!(*((resized as *const u8).add(8 + 127)), 0);
            free_byte_array(resized);
        }
    }

    #[test]
    fn test_runtime_resize_byte_array_grow() {
        unsafe {
            let ba = runtime_new_byte_array(5);
            let bytes = std::slice::from_raw_parts_mut((ba as *mut u8).add(8), 5);
            bytes.copy_from_slice(b"abcde");

            let new_ba = runtime_resize_byte_array(ba, 10);
            assert_eq!(*(new_ba as *const u64), 10);
            let new_bytes = std::slice::from_raw_parts((new_ba as *const u8).add(8), 10);
            assert_eq!(&new_bytes[0..5], b"abcde");
            assert_eq!(&new_bytes[5..10], &[0, 0, 0, 0, 0]);

            free_byte_array(new_ba);
        }
    }

    #[test]
    fn test_runtime_resize_byte_array_shrink() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            let bytes = std::slice::from_raw_parts_mut((ba as *mut u8).add(8), 10);
            bytes.copy_from_slice(b"0123456789");

            let new_ba = runtime_resize_byte_array(ba, 5);
            assert_eq!(*(new_ba as *const u64), 5);
            let new_bytes = std::slice::from_raw_parts((new_ba as *const u8).add(8), 5);
            assert_eq!(new_bytes, b"01234");

            free_byte_array(new_ba);
        }
    }

    #[test]
    fn test_runtime_copy_byte_array() {
        unsafe {
            let ba1 = runtime_new_byte_array(10);
            let ba2 = runtime_new_byte_array(10);

            let bytes1 = std::slice::from_raw_parts_mut((ba1 as *mut u8).add(8), 10);
            bytes1.copy_from_slice(b"abcdefghij");

            runtime_copy_byte_array(ba1, 2, ba2, 4, 3);

            let bytes2 = std::slice::from_raw_parts((ba2 as *const u8).add(8), 10);
            assert_eq!(&bytes2[4..7], b"cde");

            free_byte_array(ba1);
            free_byte_array(ba2);
        }
    }

    #[test]
    fn test_runtime_copy_byte_array_overlap() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            let bytes = std::slice::from_raw_parts_mut((ba as *mut u8).add(8), 10);
            bytes.copy_from_slice(b"0123456789");

            // Overlapping copy: 01234 -> 23456
            runtime_copy_byte_array(ba, 0, ba, 2, 5);

            assert_eq!(bytes, b"0101234789");

            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_compare_byte_arrays() {
        unsafe {
            let ba1 = runtime_new_byte_array(5);
            let ba2 = runtime_new_byte_array(5);

            std::ptr::copy_nonoverlapping(b"apple".as_ptr(), (ba1 as *mut u8).add(8), 5);
            std::ptr::copy_nonoverlapping(b"apply".as_ptr(), (ba2 as *mut u8).add(8), 5);

            assert_eq!(runtime_compare_byte_arrays(ba1, 0, ba2, 0, 4), 0); // "appl" == "appl"
            assert_eq!(runtime_compare_byte_arrays(ba1, 0, ba2, 0, 5), -1); // "apple" < "apply"
            assert_eq!(runtime_compare_byte_arrays(ba2, 0, ba1, 0, 5), 1); // "apply" > "apple"

            free_byte_array(ba1);
            free_byte_array(ba2);
        }
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

    #[test]
    fn test_measure_off_ascii_length() {
        // T.length "hello" = negate(measure_off(p, 0, 5, maxBound))
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, i64::MAX);
        assert_eq!(r, -5); // buffer exhausted, 5 chars found
    }

    #[test]
    fn test_measure_off_ascii_take() {
        // T.take 3 "hello" => measure_off(p, 0, 5, 3)
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 3);
        assert_eq!(r, 3); // 3 chars = 3 bytes consumed
    }

    #[test]
    fn test_measure_off_ascii_take_all() {
        // T.take 5 "hello" => cnt == total chars, returns bytes consumed
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 5);
        assert_eq!(r, 5); // exactly 5 chars fit
    }

    #[test]
    fn test_measure_off_ascii_take_more() {
        // T.take 10 "hello" => cnt > total chars, buffer exhausted
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 10);
        assert_eq!(r, -5); // only 5 chars available
    }

    #[test]
    fn test_measure_off_ascii_drop() {
        // T.drop 2 "hello" => measure_off(p, 0, 5, 2) = 2 bytes
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 2);
        assert_eq!(r, 2);
    }

    #[test]
    fn test_measure_off_with_offset() {
        // Text with off=2, len=3 (substring "llo")
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 2, 3, i64::MAX);
        assert_eq!(r, -3); // 3 chars in "llo"
    }

    #[test]
    fn test_measure_off_empty() {
        let s = b"hello";
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 0, 5), 0);
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 0), 0);
    }

    #[test]
    fn test_measure_off_utf8_length() {
        // "café" = [63 61 66 C3 A9] = 5 bytes, 4 chars
        let s = "café".as_bytes();
        assert_eq!(s.len(), 5);
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, i64::MAX);
        assert_eq!(r, -4); // 4 codepoints
    }

    #[test]
    fn test_measure_off_utf8_take() {
        // T.take 3 "café" => first 3 chars = "caf" = 3 bytes
        let s = "café".as_bytes();
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 3);
        assert_eq!(r, 3); // 3 ASCII chars = 3 bytes
    }

    #[test]
    fn test_measure_off_utf8_take_past_multibyte() {
        // T.take 4 "café" => all 4 chars, 5 bytes. cnt == total, buffer exhausted
        let s = "café".as_bytes();
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 4);
        // cnt=4, walk: c(1)+a(1)+f(1)+é(2) = 5 bytes, 4 chars found, chars_found==cnt
        assert_eq!(r, 5); // bytes consumed
    }

    #[test]
    fn test_measure_off_multibyte_chars() {
        // "λ😀x" = [CE BB | F0 9F 98 80 | 78] = 7 bytes, 3 chars
        let s = "λ😀x".as_bytes();
        assert_eq!(s.len(), 7);
        // length
        assert_eq!(
            runtime_text_measure_off(s.as_ptr() as i64, 0, 7, i64::MAX),
            -3
        );
        // take 1 = "λ" = 2 bytes
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 7, 1), 2);
        // take 2 = "λ😀" = 6 bytes
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 7, 2), 6);
        // with offset 2 (past "λ"), len 5: "😀x" = 2 chars
        assert_eq!(
            runtime_text_measure_off(s.as_ptr() as i64, 2, 5, i64::MAX),
            -2
        );
        // take 1 from offset 2: "😀" = 4 bytes
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 2, 5, 1), 4);
    }

    #[test]
    fn test_measure_off_all_widths() {
        // "Aλ文😀" = 1+2+3+4 = 10 bytes, 4 chars
        let s = "Aλ文😀".as_bytes();
        assert_eq!(s.len(), 10);
        assert_eq!(
            runtime_text_measure_off(s.as_ptr() as i64, 0, 10, i64::MAX),
            -4
        );
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 1), 1); // "A"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 2), 3); // "Aλ"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 3), 6); // "Aλ文"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 4), 10); // all
                                                                               // from offset 1 (past "A"), len 9: "λ文😀"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 1, 9, 2), 5); // "λ文" = 2+3
    }

    #[test]
    fn test_runtime_text_memchr() {
        let s = b"abacaba";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 7, b'a' as i64), 0);
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 1, 6, b'a' as i64), 1); // 'a' at index 2 of original, which is offset 1 from s+1
        assert_eq!(
            runtime_text_memchr(s.as_ptr() as i64, 0, 7, b'z' as i64),
            -1
        );
    }

    // ---------------------------------------------------------------
    // runtime_text_reverse — text-2.1.2: reverse(dst, src, off, len)
    // ---------------------------------------------------------------

    #[test]
    fn test_reverse_ascii() {
        let src = b"hello";
        let mut dest = [0u8; 5];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 5);
        assert_eq!(&dest, b"olleh");
    }

    #[test]
    fn test_reverse_ascii_with_offset() {
        // src = "XXhello", off=2, len=5 → reverse "hello" → "olleh"
        let src = b"XXhello";
        let mut dest = [0u8; 5];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 2, 5);
        assert_eq!(&dest, b"olleh");
    }

    #[test]
    fn test_reverse_utf8() {
        // "λ😀" -> CE BB | F0 9F 98 80 (6 bytes)
        // Reversed should be "😀λ" -> F0 9F 98 80 | CE BB
        let src = "λ😀".as_bytes();
        let mut dest = [0u8; 6];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 6);
        assert_eq!(std::str::from_utf8(&dest).unwrap(), "😀λ");
    }

    #[test]
    fn test_reverse_all_widths() {
        // "Aλ文😀" = 10 bytes → "😀文λA"
        let src = "Aλ文😀".as_bytes();
        let mut dest = [0u8; 10];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 10);
        assert_eq!(std::str::from_utf8(&dest).unwrap(), "😀文λA");
    }

    #[test]
    fn test_reverse_single_char() {
        let src = b"x";
        let mut dest = [0u8; 1];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 1);
        assert_eq!(&dest, b"x");
    }

    // ---------------------------------------------------------------
    // runtime_text_memchr — memchr(arr, off, len, byte) -> offset or -1
    // ---------------------------------------------------------------

    #[test]
    fn test_memchr_found() {
        let s = b"hello:world";
        assert_eq!(
            runtime_text_memchr(s.as_ptr() as i64, 0, 11, b':' as i64),
            5
        );
    }

    #[test]
    fn test_memchr_not_found() {
        let s = b"hello";
        assert_eq!(
            runtime_text_memchr(s.as_ptr() as i64, 0, 5, b':' as i64),
            -1
        );
    }

    #[test]
    fn test_memchr_with_offset() {
        let s = b"a:b:c";
        // search from offset 2 (past "a:"), len 3 ("b:c")
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 2, 3, b':' as i64), 1);
    }

    #[test]
    fn test_memchr_first_byte() {
        let s = b":hello";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 6, b':' as i64), 0);
    }

    #[test]
    fn test_memchr_last_byte() {
        let s = b"hello:";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 6, b':' as i64), 5);
    }

    // ---------------------------------------------------------------
    // decode_double_int64 — matches GHC's decodeDouble_Int64#
    // ---------------------------------------------------------------

    #[test]
    fn test_decode_double_3_14() {
        let (m, e) = decode_double_int64(3.14);
        assert_eq!(m as f64 * (2.0f64).powi(e as i32), 3.14);
    }

    #[test]
    fn test_decode_double_1_0() {
        let (m, e) = decode_double_int64(1.0);
        assert_eq!((m, e), (1, 0));
    }

    #[test]
    fn test_decode_double_42_0() {
        let (m, e) = decode_double_int64(42.0);
        assert_eq!(m as f64 * (2.0f64).powi(e as i32), 42.0);
    }

    #[test]
    fn test_decode_double_zero() {
        assert_eq!(decode_double_int64(0.0), (0, 0));
    }

    #[test]
    fn test_decode_double_negative() {
        let (m, e) = decode_double_int64(-1.5);
        assert_eq!((m, e), (-3, -1));
    }

    #[test]
    fn test_decode_double_runtime_mantissa() {
        let bits = 3.14f64.to_bits() as i64;
        let m = runtime_decode_double_mantissa(bits);
        let e = runtime_decode_double_exponent(bits);
        assert_eq!(m as f64 * (2.0f64).powi(e as i32), 3.14);
    }
}
