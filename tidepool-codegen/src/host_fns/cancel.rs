//! External-cancellation safepoint: the check JIT code and other host fns
//! consult to unwind promptly. The flag itself lives on the per-machine
//! `MachineState` (installed by `JitEffectMachine::install_registries`,
//! cleared by `RegistryGuard::drop`); this module holds only the check.

use crate::context::VMContext;
use crate::machine_state::machine_state;

use super::errors::{error_poison_ptr, RuntimeError};

/// If cancellation has been requested, record `RuntimeError::Cancelled`
/// (unless another error is already pending) and return `true`. Callers
/// should then unwind by returning a poison pointer from their loop so the
/// outer run loop can surface the error.
///
/// # Safety
/// `vmctx` must be non-null with `machine_state` installed.
#[inline]
pub(crate) fn check_cancel_and_set_error(vmctx: *mut VMContext) -> bool {
    // SAFETY: caller contract above.
    if unsafe { machine_state(vmctx) }.cancel_requested() {
        super::errors::set_first_cause(RuntimeError::Cancelled);
        true
    } else {
        false
    }
}

/// External-cancellation safepoint for recursive join-point back-edges.
///
/// Called by JIT code immediately before a `jump` that closes a recursive,
/// non-tail, non-allocating join loop. Such a loop reaches none of the other
/// cancel safepoints (trampoline tail calls, `gc_trigger`, the effect-dispatch
/// boundary) — without a check here, a pending cancel is never observed and
/// the loop wedges forever instead of unwinding.
///
/// Returns `null` to continue the loop, or the error poison pointer (with
/// `RuntimeError::Cancelled` recorded via `check_cancel_and_set_error`) when a
/// cancel is pending. The compiled back-edge `brif`s on the result: non-null
/// unwinds by returning the poison from the current function.
#[inline]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn runtime_cancel_check(vmctx: *mut VMContext) -> *mut u8 {
    if check_cancel_and_set_error(vmctx) {
        error_poison_ptr()
    } else {
        std::ptr::null_mut()
    }
}
