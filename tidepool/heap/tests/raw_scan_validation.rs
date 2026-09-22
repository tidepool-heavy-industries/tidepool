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
        for_each_pointer_field(ptr, 256, |p| seen.push(*p));
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
        for_each_pointer_field(ptr, 256, |p| seen.push(*p));
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
        for_each_pointer_field(ptr, 64, |p| seen.push(*p));
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
        for_each_pointer_field(ptr, 64, |p| seen.push(*p));
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
        for_each_pointer_field(ptr, 64, |_| {});
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
        for_each_pointer_field(ptr, 64, |_| count += 1);
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
        for_each_pointer_field(ptr, 64, |_| {});
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
        for_each_pointer_field(ptr, 64, |p| seen.push(*p));
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
        for_each_pointer_field(ptr, 64, |_| {});
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
        for_each_pointer_field(ptr, 64, |p| seen.push(*p));
        assert!(
            seen.is_empty(),
            "an inflated num_captured must not be trusted; got {seen:?}"
        );
    }
}

// ── Negative: any Thunk below the canonical THUNK_MIN_SIZE (24) ────────────
//
// The collector enforces ONE minimum thunk size regardless of state —
// matching layout.rs's THUNK_MIN_SIZE — checked before the state byte or any
// capture count is derived. A thunk below it (whatever state byte it
// happens to carry, including a stale/corrupted one) is rejected outright;
// there is no smaller legitimate shape for any thunk state, including
// BlackHole (an in-progress evaluation with zero captures visited is exactly
// THUNK_MIN_SIZE, never less).

#[test]
#[should_panic(expected = "THUNK_MIN_SIZE")]
fn undersized_thunk_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        // Below THUNK_MIN_SIZE (24) but still >= HEADER_SIZE (8), so this
        // exercises the thunk-specific floor, not the generic header check.
        let ptr = write_evaluated_thunk(&mut buf.0, THUNK_STATE_OFFSET as u32 + 1);
        for_each_pointer_field(ptr, 64, |_| {});
    }
}

#[test]
fn undersized_thunk_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = write_evaluated_thunk(&mut buf.0, THUNK_STATE_OFFSET as u32 + 1);
        let mut count = 0;
        for_each_pointer_field(ptr, 64, |_| count += 1);
        assert_eq!(
            count, 0,
            "a thunk below THUNK_MIN_SIZE must not have any slot read"
        );
    }
}

#[test]
fn minimum_sized_blackhole_thunk_has_no_captures() {
    // THUNK_MIN_SIZE (24) is the smallest LEGAL thunk: a BlackHole with zero
    // captures visited so far. This is the positive control proving the
    // THUNK_MIN_SIZE floor above doesn't reject legitimate minimum-sized
    // thunks — companion to `undersized_thunk_*` (23 bytes, one less, is
    // rejected).
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    let mut buf = AlignedBuf::<64>([0u8; 64]);
    unsafe {
        let ptr = buf.0.as_mut_ptr();
        write_header(ptr, TAG_THUNK, THUNK_MIN_SIZE as u32);
        *ptr.add(THUNK_STATE_OFFSET) = THUNK_BLACKHOLE;
        let mut count = 0;
        for_each_pointer_field(ptr, 64, |_| count += 1);
        assert_eq!(count, 0, "a minimum-sized BlackHole has no captures yet");
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
        for_each_pointer_field(ptr, 64, |_| {});
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
        for_each_pointer_field(ptr, 64, |_| count += 1);
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
        for_each_pointer_field(ptr, 64, |_| {});
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
        for_each_pointer_field(ptr2, 64, |_| count += 1);
        assert_eq!(count, 0);
    }
}

// ── Negative: object at the very end of its allocation (the `avail` bound
//    itself, independent of `size`) ────────────────────────────────────────
//
// Every case above uses a buffer strictly larger than the declared object,
// so a bug that read based on `size` alone (ignoring the real allocation
// extent) would go undetected — the over-read would land in buffer slack
// rather than past the allocation. These cases place the object at the
// EXACT end of an exactly-sized allocation (`avail` passed to
// `for_each_pointer_field` equals the allocation's true length, with zero
// slack) and corrupt a count so satisfying it would read past that
// allocation. A regression that re-introduces "trust `size`/the count,
// ignore `avail`" would read past `buf`'s real extent here — this proves the
// diagnostic fires from the bound itself, not from `size` happening to also
// be small.

#[test]
#[should_panic(expected = "only 24 bytes are readable")]
fn con_num_fields_exceeding_exact_allocation_panics_in_diagnostic_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    // Exactly CON_FIELDS_OFFSET (24) bytes — room for the header + con_tag +
    // num_fields, and NOTHING else. No field slot fits.
    let mut buf = AlignedBuf::<{ CON_FIELDS_OFFSET }>([0u8; CON_FIELDS_OFFSET]);
    unsafe {
        // Declared size lies (claims room for 2 fields) but the real
        // allocation — and the `avail` passed below — has none.
        let declared_size = (CON_FIELDS_OFFSET + 2 * FIELD_STRIDE) as u32;
        let ptr = write_con(&mut buf.0, 1, declared_size, 2);
        for_each_pointer_field(ptr, CON_FIELDS_OFFSET, |_| {});
    }
}

#[test]
fn con_num_fields_exceeding_exact_allocation_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<{ CON_FIELDS_OFFSET }>([0u8; CON_FIELDS_OFFSET]);
    unsafe {
        let declared_size = (CON_FIELDS_OFFSET + 2 * FIELD_STRIDE) as u32;
        let ptr = write_con(&mut buf.0, 1, declared_size, 2);
        let mut count = 0;
        for_each_pointer_field(ptr, CON_FIELDS_OFFSET, |_| count += 1);
        assert_eq!(
            count, 0,
            "a count exceeding the real allocation extent must not be trusted, \
             even though `size` alone claims enough room"
        );
    }
}

#[test]
#[should_panic(expected = "avail=16")]
fn closure_num_captured_field_unreadable_at_exact_allocation_end() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(true);
    // Exactly CLOSURE_NUM_CAPTURED_OFFSET (16) bytes: room for the header
    // and code_ptr, but not even the 2-byte num_captured field itself — the
    // metadata pre-read guard must fire before any dereference, not just the
    // post-hoc needed-vs-avail check.
    let mut buf = AlignedBuf::<{ CLOSURE_NUM_CAPTURED_OFFSET }>([0u8; CLOSURE_NUM_CAPTURED_OFFSET]);
    unsafe {
        let ptr = buf.0.as_mut_ptr();
        // Declare a size that (falsely) claims the num_captured field fits.
        write_header(ptr, TAG_CLOSURE, (CLOSURE_NUM_CAPTURED_OFFSET + 2) as u32);
        *(ptr.add(CLOSURE_CODE_PTR_OFFSET) as *mut usize) = 0x1234;
        for_each_pointer_field(ptr, CLOSURE_NUM_CAPTURED_OFFSET, |_| {});
    }
}

#[test]
fn closure_num_captured_field_unreadable_at_exact_allocation_end_skips_in_normal_mode() {
    let _guard = CheckedScanningGuard;
    set_checked_scanning(false);
    let mut buf = AlignedBuf::<{ CLOSURE_NUM_CAPTURED_OFFSET }>([0u8; CLOSURE_NUM_CAPTURED_OFFSET]);
    unsafe {
        let ptr = buf.0.as_mut_ptr();
        write_header(ptr, TAG_CLOSURE, (CLOSURE_NUM_CAPTURED_OFFSET + 2) as u32);
        *(ptr.add(CLOSURE_CODE_PTR_OFFSET) as *mut usize) = 0x1234;
        let mut count = 0;
        for_each_pointer_field(ptr, CLOSURE_NUM_CAPTURED_OFFSET, |_| count += 1);
        assert_eq!(
            count, 0,
            "an unreadable num_captured field must not be dereferenced"
        );
    }
}
