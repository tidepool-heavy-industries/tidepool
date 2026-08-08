//! `for_each_pointer_field` validates the size/count relationship of every
//! tag arm before it constructs a raw field-slot pointer from a derived
//! count (`num_captured`, `num_fields`, a Thunk's own size, or a boxed
//! array's payload `len`). In diagnostic mode (`set_checked_scanning(true)`,
//! mirroring `TIDEPOOL_HEAP_VERIFY`) a violation panics with the object's
//! tag, size, and first bytes; in normal mode the object's fields are
//! skipped instead of iterated, with an always-on stderr breadcrumb.
//!
//! Each negative case hand-builds an object whose stored count claims more
//! than its declared size permits, then exercises both modes. A positive
//! control per tag proves a well-formed object of the same shape still scans
//! every field — the negative cases would prove nothing if validation also
//! rejected legitimate objects.

use std::panic::{catch_unwind, AssertUnwindSafe};
use tidepool_heap::gc::raw::{
    clear_checked_scanning_override, for_each_pointer_field, set_checked_scanning,
};
use tidepool_heap::layout::*;

#[repr(align(8))]
struct AlignedBuf<const N: usize>([u8; N]);

/// Clears the checked-scanning override on drop (including panic unwind),
/// returning to the (tri-state) unset default so a test can't leak its mode
/// into whatever runs next in the same process. Unlike `set_checked_scanning
/// (false)`, clearing the override defers back to `TIDEPOOL_HEAP_VERIFY`
/// rather than forcing normal mode — the correct reset for a guard, since
/// "reset" should mean "no longer testing", not "force off, ignore the env".
struct CheckedScanningGuard;
impl Drop for CheckedScanningGuard {
    fn drop(&mut self) {
        clear_checked_scanning_override();
    }
}

unsafe fn write_con(buf: &mut [u8], con_tag: u64, declared_size: u32, num_fields: u16) -> *mut u8 {
    let ptr = buf.as_mut_ptr();
    write_header(ptr, TAG_CON, declared_size);
    *(ptr.add(CON_TAG_OFFSET) as *mut u64) = con_tag;
    *(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = num_fields;
    ptr
}

unsafe fn write_closure(buf: &mut [u8], declared_size: u32, num_captured: u16) -> *mut u8 {
    let ptr = buf.as_mut_ptr();
    write_header(ptr, TAG_CLOSURE, declared_size);
    *(ptr.add(CLOSURE_CODE_PTR_OFFSET) as *mut usize) = 0x1234;
    *(ptr.add(CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = num_captured;
    ptr
}

unsafe fn write_evaluated_thunk(buf: &mut [u8], declared_size: u32) -> *mut u8 {
    let ptr = buf.as_mut_ptr();
    write_header(ptr, TAG_THUNK, declared_size);
    *ptr.add(THUNK_STATE_OFFSET) = THUNK_EVALUATED;
    ptr
}

unsafe fn write_smallarray_lit(buf: &mut [u8], payload: *mut u8) -> *mut u8 {
    let ptr = buf.as_mut_ptr();
    write_header(ptr, TAG_LIT, LIT_SIZE as u32);
    *ptr.add(LIT_TAG_OFFSET) = LitTag::SmallArray as u8;
    *(ptr.add(LIT_VALUE_OFFSET) as *mut *mut u8) = payload;
    ptr
}

// ── Positive controls: well-formed objects still scan every field ─────────

#[test]
fn well_formed_con_scans_every_field() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<256>([0u8; 256]);
    unsafe {
        let size = (CON_FIELDS_OFFSET + 3 * FIELD_STRIDE) as u32;
        let ptr = write_con(&mut buf.0, 7, size, 3);
        for i in 0..3 {
            *(ptr.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) =
                (0x1000 + i * 8) as *mut u8;
        }
        let mut seen = Vec::new();
        for_each_pointer_field(ptr, |p| seen.push(*p));
        assert_eq!(
            seen,
            vec![0x1000 as *mut u8, 0x1008 as *mut u8, 0x1010 as *mut u8],
            "a well-formed Con must still have every field visited"
        );
    }
}

#[test]
fn well_formed_closure_scans_every_field() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<256>([0u8; 256]);
    unsafe {
        let size = (CLOSURE_CAPTURED_OFFSET + 2 * FIELD_STRIDE) as u32;
        let ptr = write_closure(&mut buf.0, size, 2);
        for i in 0..2 {
            *(ptr.add(CLOSURE_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) =
                (0x2000 + i * 8) as *mut u8;
        }
        let mut seen = Vec::new();
        for_each_pointer_field(ptr, |p| seen.push(*p));
        assert_eq!(
            seen,
            vec![0x2000 as *mut u8, 0x2008 as *mut u8],
            "a well-formed Closure must still have every capture visited"
        );
    }
}

#[test]
fn well_formed_evaluated_thunk_scans_indirection() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let size = (THUNK_INDIRECTION_OFFSET + FIELD_STRIDE) as u32;
        let ptr = write_evaluated_thunk(&mut buf.0, size);
        *(ptr.add(THUNK_INDIRECTION_OFFSET) as *mut *mut u8) = 0x3000 as *mut u8;
        let mut seen = Vec::new();
        for_each_pointer_field(ptr, |p| seen.push(*p));
        assert_eq!(
            seen,
            vec![0x3000 as *mut u8],
            "a well-formed evaluated Thunk must have its indirection slot visited"
        );
    }
}

#[test]
fn well_formed_boxed_array_scans_every_slot() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut payload_buf = AlignedBuf::<64>([0u8; 64]);
    let mut lit_buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let payload = payload_buf.0.as_mut_ptr();
        *(payload as *mut u64) = 2;
        *(payload.add(8) as *mut *mut u8) = 0x4000 as *mut u8;
        *(payload.add(16) as *mut *mut u8) = 0x4008 as *mut u8;

        let ptr = write_smallarray_lit(&mut lit_buf.0, payload);
        let mut seen = Vec::new();
        for_each_pointer_field(ptr, |p| seen.push(*p));
        assert_eq!(
            seen,
            vec![0x4000 as *mut u8, 0x4008 as *mut u8],
            "a well-formed boxed array must have every payload slot visited"
        );
    }
}

// ── Negative: size below the header minimum ────────────────────────────────

#[test]
#[should_panic(expected = "size below header minimum")]
fn size_below_header_minimum_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        // A header must be at least 8 bytes; declare a corrupted size of 4.
        let ptr = buf.0.as_mut_ptr();
        write_header(ptr, TAG_CON, 4);
        for_each_pointer_field(ptr, |_| {});
    }
}

#[test]
fn size_below_header_minimum_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = buf.0.as_mut_ptr();
        write_header(ptr, TAG_CON, 4);
        let mut count = 0;
        for_each_pointer_field(ptr, |_| count += 1);
        assert_eq!(
            count, 0,
            "an object with a degenerate size must not be scanned"
        );
    }
}

// ── Negative: Con num_fields inflated past what size permits ───────────────

#[test]
#[should_panic(expected = "Con num_fields=5")]
fn con_inflated_num_fields_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        // Declared size accommodates zero fields, but num_fields claims 5 —
        // scanning it as declared would read three field slots past the
        // object's real 24-byte extent.
        let ptr = write_con(&mut buf.0, 1, CON_FIELDS_OFFSET as u32, 5);
        for_each_pointer_field(ptr, |_| {});
    }
}

#[test]
fn con_inflated_num_fields_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = write_con(&mut buf.0, 1, CON_FIELDS_OFFSET as u32, 5);
        // Poison the region a naive scan would read, so a wrongly-executed
        // iteration would show up in `seen` instead of silently matching.
        for i in 0..5usize {
            *(ptr.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) =
                (0xdead0000 + i * 8) as *mut u8;
        }
        let mut seen = Vec::new();
        for_each_pointer_field(ptr, |p| seen.push(*p));
        assert!(
            seen.is_empty(),
            "an inflated num_fields must not be trusted; got {seen:?}"
        );
    }
}

// ── Negative: Closure num_captured inflated past what size permits ─────────

#[test]
#[should_panic(expected = "Closure num_captured=3")]
fn closure_inflated_num_captured_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = write_closure(&mut buf.0, CLOSURE_CAPTURED_OFFSET as u32, 3);
        for_each_pointer_field(ptr, |_| {});
    }
}

#[test]
fn closure_inflated_num_captured_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = write_closure(&mut buf.0, CLOSURE_CAPTURED_OFFSET as u32, 3);
        for i in 0..3usize {
            *(ptr.add(CLOSURE_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) =
                (0xdead0000 + i * 8) as *mut u8;
        }
        let mut seen = Vec::new();
        for_each_pointer_field(ptr, |p| seen.push(*p));
        assert!(
            seen.is_empty(),
            "an inflated num_captured must not be trusted; got {seen:?}"
        );
    }
}

// ── Negative: evaluated Thunk truncated below its required indirection slot ─

#[test]
#[should_panic(expected = "Thunk (Evaluated)")]
fn evaluated_thunk_truncated_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        // An Evaluated thunk always needs the indirection slot — unlike
        // Unevaluated/BlackHole, there is no legitimate reason for this to
        // be undersized.
        let ptr = write_evaluated_thunk(&mut buf.0, THUNK_STATE_OFFSET as u32 + 1);
        for_each_pointer_field(ptr, |_| {});
    }
}

#[test]
fn evaluated_thunk_truncated_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = write_evaluated_thunk(&mut buf.0, THUNK_STATE_OFFSET as u32 + 1);
        let mut count = 0;
        for_each_pointer_field(ptr, |_| count += 1);
        assert_eq!(
            count, 0,
            "a truncated evaluated Thunk must not have its (absent) indirection slot read"
        );
    }
}

// ── Negative: boxed array len whose byte span overflows ────────────────────

#[test]
#[should_panic(expected = "overflows its byte-span computation")]
fn boxed_array_len_overflow_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut payload_buf = AlignedBuf::<64>([0u8; 64]);
    let mut lit_buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let payload = payload_buf.0.as_mut_ptr();
        // A len this large can never have a real backing allocation; its
        // byte-span computation (8 + len*8) overflows usize on any real
        // target. The panic must fire from the overflow check alone, before
        // any attempt to read past the 8-byte length prefix.
        *(payload as *mut u64) = u64::MAX;
        let ptr = write_smallarray_lit(&mut lit_buf.0, payload);
        for_each_pointer_field(ptr, |_| {});
    }
}

#[test]
fn boxed_array_len_overflow_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut payload_buf = AlignedBuf::<64>([0u8; 64]);
    let mut lit_buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let payload = payload_buf.0.as_mut_ptr();
        *(payload as *mut u64) = u64::MAX;
        let ptr = write_smallarray_lit(&mut lit_buf.0, payload);
        let mut count = 0;
        for_each_pointer_field(ptr, |_| count += 1);
        assert_eq!(
            count, 0,
            "an overflowing boxed-array len must not be trusted; nothing should be iterated"
        );
    }
}

// ── The diagnostic-mode panic path is exercised via catch_unwind too, to
//    prove it is a real panic (not e.g. a process abort) and that the
//    process remains usable afterward. ────────────────────────────────────

#[test]
fn diagnostic_panic_is_a_catchable_unwind_not_a_process_abort() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    let ptr = buf.0.as_mut_ptr() as usize;
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let ptr = ptr as *mut u8;
        write_con(
            std::slice::from_raw_parts_mut(ptr, 64),
            1,
            CON_FIELDS_OFFSET as u32,
            9,
        );
        for_each_pointer_field(ptr, |_| {});
    }));
    assert!(
        result.is_err(),
        "a size/count containment violation in diagnostic mode must panic"
    );

    // Prove the process is still usable: a fresh, well-formed object scans
    // correctly right after the caught panic.
    set_checked_scanning(false);
    let mut buf2 = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr2 = write_con(&mut buf2.0, 2, CON_FIELDS_OFFSET as u32, 0);
        let mut count = 0;
        for_each_pointer_field(ptr2, |_| count += 1);
        assert_eq!(count, 0);
    }
}
