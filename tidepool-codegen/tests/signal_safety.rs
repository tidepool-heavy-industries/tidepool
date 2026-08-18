//! Test that sigsetjmp/siglongjmp signal protection actually works.
//!
//! These tests MUST NOT run concurrently: the signal protection uses a global
//! JMP_BUF, so concurrent signal-catching tests will race and crash.
//! A shared mutex serializes them.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static SIGNAL_LOCK: Mutex<()> = Mutex::new(());

/// Trigger an illegal instruction (SIGILL).
/// Separate function to prevent the compiler from optimizing away the fault.
#[inline(never)]
unsafe fn trigger_sigill() {
    #[cfg(target_arch = "x86_64")]
    std::arch::asm!("ud2");
    #[cfg(target_arch = "aarch64")]
    std::arch::asm!("udf #0");
}

#[test]
fn test_sigill_returns_signal_error() {
    let _lock = SIGNAL_LOCK.lock();
    tidepool_codegen::signal_safety::install();

    let result = unsafe {
        tidepool_codegen::signal_safety::with_signal_protection(|| {
            trigger_sigill();
        })
    };

    let Err(e) = result else {
        panic!("expected SignalError, got Ok");
    };
    assert_eq!(e.0, libc::SIGILL, "expected SIGILL, got signal {}", e.0);
    eprintln!("Signal caught correctly: {}", e);
}

#[test]
fn test_normal_execution_returns_ok() {
    let _lock = SIGNAL_LOCK.lock();
    tidepool_codegen::signal_safety::install();

    let result = unsafe { tidepool_codegen::signal_safety::with_signal_protection(|| 42i32) };

    assert_eq!(result.unwrap(), 42);
}

#[test]
fn test_signal_recovery_allows_subsequent_calls() {
    let _lock = SIGNAL_LOCK.lock();
    tidepool_codegen::signal_safety::install();

    // First call: crash
    let result1 = unsafe {
        tidepool_codegen::signal_safety::with_signal_protection(|| {
            trigger_sigill();
        })
    };
    assert!(result1.is_err());

    // Second call: should still work
    let result2 = unsafe { tidepool_codegen::signal_safety::with_signal_protection(|| 100i32) };
    assert_eq!(result2.unwrap(), 100);
}

/// Bumps an `AtomicUsize` on drop, so tests can observe how many times a
/// closure's captured environment was actually destructed.
struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_fault_mid_closure_never_double_drops_the_capture() {
    let _lock = SIGNAL_LOCK.lock();
    tidepool_codegen::signal_safety::install();

    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let counter = DropCounter(&COUNT);

    let result = unsafe {
        tidepool_codegen::signal_safety::with_signal_protection(move || {
            let _held = &counter;
            trigger_sigill();
        })
    };

    assert!(result.is_err(), "expected SignalError, got Ok");
    let drops = COUNT.load(Ordering::SeqCst);
    // The trampoline moves the closure out of the caller's payload onto its
    // own stack frame before invoking it; a fault mid-call abandons that
    // frame via siglongjmp, which skips Rust destructors, so the capture
    // leaks — exactly 0 drops. The caller's payload no longer owns the
    // closure by this point, so it has nothing left to drop either. Any
    // observed drop would mean something still (wrongly) owns and drops a
    // value the trampoline already owns.
    assert_eq!(
        drops, 0,
        "capture dropped {} times, expected exactly 0 (leak-on-fault, never dropped)",
        drops
    );
}

#[test]
fn test_normal_completion_drops_capture_exactly_once() {
    let _lock = SIGNAL_LOCK.lock();
    tidepool_codegen::signal_safety::install();

    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let counter = DropCounter(&COUNT);

    let result = unsafe {
        tidepool_codegen::signal_safety::with_signal_protection(move || {
            let _held = &counter;
            7i32
        })
    };

    assert_eq!(result.unwrap(), 7);
    assert_eq!(
        COUNT.load(Ordering::SeqCst),
        1,
        "capture should be dropped exactly once on the normal path"
    );
}

#[test]
fn test_non_copy_result_round_trips() {
    let _lock = SIGNAL_LOCK.lock();
    tidepool_codegen::signal_safety::install();

    let result = unsafe {
        tidepool_codegen::signal_safety::with_signal_protection(|| {
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        })
    };

    assert_eq!(
        result.unwrap(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}
