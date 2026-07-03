//! External-cancellation flag: install/clear the thread-local flag, and the
//! safepoint checks JIT code and other host fns consult to unwind promptly.

use crate::context::VMContext;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::errors::{error_poison_ptr, RuntimeError};

thread_local! {
    /// External cancellation flag. When set, the next GC safepoint will abort the
    /// running program with `RuntimeError::Cancelled`. Cloned from the
    /// `Arc<AtomicBool>` owned by the `JitEffectMachine` before entering JIT code.
    ///
    /// Installed by `set_cancel_flag` (called from `JitEffectMachine::install_registries`)
    /// and cleared by `clear_cancel_flag` (called from `RegistryGuard::drop`).
    static CANCEL_FLAG: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Install an external cancellation flag for the current thread. The next
/// GC safepoint (heap check) will observe the flag and abort the program with
/// `RuntimeError::Cancelled` if it has been set to `true`.
///
/// Called from `JitEffectMachine::install_registries` before entering JIT code.
pub(crate) fn set_cancel_flag(flag: Arc<AtomicBool>) {
    CANCEL_FLAG.with(|cell| {
        *cell.borrow_mut() = Some(flag);
    });
}

/// Remove the installed cancellation flag for the current thread. Called from
/// `RegistryGuard::drop` so the Arc is released even on an early error return.
pub(crate) fn clear_cancel_flag() {
    CANCEL_FLAG.with(|cell| {
        cell.borrow_mut().take();
    });
}

/// Fast check for an external cancel request. Uses a relaxed load — the cost
/// of a single extra relaxed atomic load per heap check is negligible, and
/// cancellation is best-effort (observed at the next safepoint) so stronger
/// ordering is not required.
#[inline]
fn cancel_requested() -> bool {
    CANCEL_FLAG.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    })
}

/// If cancellation has been requested, record `RuntimeError::Cancelled`
/// (unless another error is already pending) and return `true`. Callers
/// should then unwind by returning a poison pointer from their loop so the
/// outer run loop can surface the error.
#[inline]
pub(crate) fn check_cancel_and_set_error() -> bool {
    if cancel_requested() {
        super::errors::set_first_cause(RuntimeError::Cancelled);
        true
    } else {
        false
    }
}

/// External-cancellation safepoint for recursive join-point back-edges (#325).
///
/// Called by JIT code immediately before a `jump` that closes a **recursive**
/// join loop (a GHC-loopified non-tail, non-allocating spin). Such a loop
/// reaches none of the other three cancel safepoints — the trampoline
/// (`trampoline_resolve`, tail calls), `gc_trigger` (allocating loops, #273),
/// and the effect-dispatch boundary — so without a check here the 30s timeout
/// sets the cancel flag but nothing observes it and the loop wedges.
///
/// Returns `null` to continue the loop, or the error poison pointer (with
/// `RuntimeError::Cancelled` recorded via `check_cancel_and_set_error`) when a
/// cancel is pending. The compiled back-edge `brif`s on the result: non-null
/// unwinds by returning the poison from the current function, mirroring the
/// trampoline's check one layer down. `vmctx` is unused (the cancel flag is a
/// thread-local) but kept in the ABI for uniformity with the other host fns.
#[inline]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn runtime_cancel_check(_vmctx: *mut VMContext) -> *mut u8 {
    if check_cancel_and_set_error() {
        error_poison_ptr()
    } else {
        std::ptr::null_mut()
    }
}
