//! L3 (repo-review-2026-07-06/01-gc-memory-safety.md, Low findings):
//! `runtime_shape_trap`'s diagnostic dump unconditionally read 32 bytes from
//! the scrutinee, but every heap object is only GUARANTEED to be 24 bytes
//! (Lit's total size). A 24-byte `Lit` sitting at the very end of the
//! nursery made that an 8-byte out-of-bounds read past the allocation.
//!
//! Reproduces this with an actual guard page (mmap two pages, `PROT_NONE`
//! the second) and a `Lit` object placed so its last byte lands exactly on
//! the page boundary — the old 32-byte read would run 8 bytes into the
//! unmapped page and SIGSEGV; the fix clamps to the guaranteed-safe 24.
//!
//! Uses `signal_safety::with_signal_protection` (like `tests/signal_safety.rs`)
//! so a regression is a caught `Err(SignalError)`, not a crashed test binary.
//! Shares that file's `SIGNAL_LOCK` discipline (global JMP_BUF — signal tests
//! must not run concurrently with each other).

use std::sync::Mutex;

static SIGNAL_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn shape_trap_dump_does_not_read_past_a_page_boundary() {
    let _lock = SIGNAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tidepool_codegen::signal_safety::install();

    unsafe {
        let page_size = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let map_size = page_size * 2;
        let base = libc::mmap(
            std::ptr::null_mut(),
            map_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(base, libc::MAP_FAILED, "mmap failed");

        // Guard page: any read into it faults.
        let guard_page = (base as *mut u8).add(page_size);
        let mprotect_rc =
            libc::mprotect(guard_page as *mut libc::c_void, page_size, libc::PROT_NONE);
        assert_eq!(mprotect_rc, 0, "mprotect failed");

        // A 24-byte Lit ending exactly at the page boundary.
        const LIT_TOTAL_SIZE: usize = 24;
        let obj_ptr = (base as *mut u8).add(page_size - LIT_TOTAL_SIZE);
        tidepool_heap::layout::write_header(
            obj_ptr,
            tidepool_codegen::layout::TAG_LIT,
            LIT_TOTAL_SIZE as u32,
        );
        *obj_ptr.add(tidepool_codegen::layout::LIT_TAG_OFFSET as usize) = 0; // Int#
        *(obj_ptr.add(tidepool_codegen::layout::LIT_VALUE_OFFSET as usize) as *mut i64) = 42;

        let result = tidepool_codegen::signal_safety::with_signal_protection(|| {
            tidepool_codegen::host_fns::runtime_shape_trap(
                tidepool_codegen::host_fns::ShapeTrapKind::CaseMiss as i64,
                obj_ptr as i64,
                0,
                0,
                0,
                0,
            )
        });

        libc::munmap(base, map_size);

        assert!(
            result.is_ok(),
            "shape-trap dump read past the 24-byte Lit into the guard page: {result:?}"
        );
    }
}
