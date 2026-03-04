//! Test that sigsetjmp/siglongjmp signal protection actually works.
//!
//! These tests MUST NOT run concurrently: the signal protection uses a global
//! JMP_BUF, so concurrent signal-catching tests will race and crash.
//! A shared mutex serializes them.

use parking_lot::Mutex;

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

    match result {
        Err(e) => {
            assert_eq!(e.0, libc::SIGILL, "expected SIGILL, got signal {}", e.0);
            eprintln!("Signal caught correctly: {}", e);
        }
        Ok(()) => panic!("expected SignalError, got Ok"),
    }
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
