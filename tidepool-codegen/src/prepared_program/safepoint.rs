//! Prepared safepoints use the invocation machine, never legacy TLS state.

use crate::context::VMContext;
use crate::host_fns::RuntimeError;
use crate::machine_state::machine_state;

/// Native bounds for the current invocation thread. The limit includes native
/// host/unwind headroom; generated entry checks additionally reserve the
/// program's largest finalized frame before permitting another generated call.
pub(super) struct NativeStackBounds {
    pub low: usize,
    pub high: usize,
}

impl NativeStackBounds {
    pub(super) const UNWIND_RESERVE: usize = 64 * 1024;

    /// Convert an OS stack bound into the VMContext limit consumed by a
    /// generated prepared-entry preflight. A limit at or above the stack's
    /// usable high end cannot leave room for a native frame and is rejected
    /// before entering generated code.
    pub(super) fn limit_with_frame_reserve(
        &self,
        frame_reserve: usize,
    ) -> Result<*const u8, RuntimeError> {
        let limit = self
            .low
            .checked_add(frame_reserve)
            .filter(|limit| *limit < self.high)
            .ok_or(RuntimeError::StackOverflow)?;
        Ok(limit as *const u8)
    }

    /// Check the caller's current native SP before entering the platform
    /// adapter. This is intentionally separate from the generated preflight:
    /// the adapter itself has not yet had a chance to run its check here.
    pub(super) fn ensure_current_frame_reserve(
        &self,
        frame_reserve: usize,
    ) -> Result<(), RuntimeError> {
        let limit = self
            .low
            .checked_add(frame_reserve)
            .filter(|limit| *limit < self.high)
            .ok_or(RuntimeError::StackOverflow)?;
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let stack_pointer = {
            let stack_pointer: usize;
            // SAFETY: reading RSP does not touch memory or change machine
            // state; this check runs before any generated adapter call.
            unsafe {
                std::arch::asm!(
                    "mov {}, rsp",
                    out(reg) stack_pointer,
                    options(nomem, nostack, preserves_flags)
                );
            }
            stack_pointer
        };
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let stack_pointer = return Err(RuntimeError::StackOverflow);
        if stack_pointer < limit {
            Err(RuntimeError::StackOverflow)
        } else {
            Ok(())
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub(super) fn current() -> Result<Self, RuntimeError> {
        let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        // The OS owns the current thread; successful getattr initializes attr.
        unsafe {
            if libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()) != 0 {
                return Err(RuntimeError::StackOverflow);
            }
            let mut attributes = attributes.assume_init();
            let mut address = std::ptr::null_mut();
            let mut size = 0;
            let mut guard = 0;
            let stack_result = libc::pthread_attr_getstack(&attributes, &mut address, &mut size);
            let guard_result = libc::pthread_attr_getguardsize(&attributes, &mut guard);
            libc::pthread_attr_destroy(&mut attributes);
            if stack_result != 0 || guard_result != 0 {
                return Err(RuntimeError::StackOverflow);
            }
            let high = (address as usize)
                .checked_add(size)
                .ok_or(RuntimeError::StackOverflow)?;
            let low = (address as usize)
                .checked_add(guard)
                .and_then(|low| low.checked_add(Self::UNWIND_RESERVE))
                .filter(|low| *low < high)
                .ok_or(RuntimeError::StackOverflow)?;
            Ok(Self { low, high })
        }
    }

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    pub(super) fn current() -> Result<Self, RuntimeError> {
        Err(RuntimeError::StackOverflow)
    }
}

/// Return the current ABI status after sampling cancellation. Callers branch
/// before reading payloads or mutating the heap. The VMContext owns the machine
/// association even when another legacy machine is installed on the thread.
///
/// # Safety
/// `vmctx` and its machine must remain valid throughout the call.
pub(super) unsafe extern "C" fn prepared_poll(vmctx: *mut VMContext) -> i32 {
    let machine = unsafe { machine_state(vmctx) };
    if machine.cancel_requested() {
        machine.set_first_cause(RuntimeError::Cancelled);
    }
    machine.prepared_call_status() as i32
}

/// Record a native stack preflight failure against the invocation machine.
/// Generated code calls this only after comparing its own SP with the
/// VMContext threshold, so no signal/trap path is needed near the guard page.
pub(super) unsafe extern "C" fn prepared_stack_overflow(vmctx: *mut VMContext) -> i32 {
    let machine = unsafe { machine_state(vmctx) };
    machine.set_first_cause(RuntimeError::StackOverflow);
    machine.prepared_call_status() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_state::MachineState;
    use crate::prepared_control::CallStatus;
    use std::sync::{atomic::AtomicBool, Arc};

    unsafe extern "C" fn no_gc(_: *mut VMContext) {}

    #[test]
    fn w5_a1_poll_records_on_invocation_without_tls() {
        let machine = MachineState::new();
        machine.set_cancel_flag(Arc::new(AtomicBool::new(true)));
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null(), no_gc);
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        let status = unsafe { prepared_poll(&mut vmctx) };
        assert_ne!(status, CallStatus::Success as i32);
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }

    #[test]
    fn w5_a1_poll_preserves_terminal_first_cause() {
        let machine = MachineState::new();
        machine.set_first_cause(RuntimeError::BadPointer);
        machine.set_cancel_flag(Arc::new(AtomicBool::new(true)));
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null(), no_gc);
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        assert_eq!(
            unsafe { prepared_poll(&mut vmctx) },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn w5_a1_native_bounds_small_stack() {
        let join = std::thread::Builder::new()
            .name("prepared-small-stack".into())
            .stack_size(256 * 1024)
            .spawn(|| NativeStackBounds::current().map(|bounds| (bounds.low, bounds.high)))
            .expect("small-stack thread should start")
            .join()
            .expect("small-stack thread should return");
        let (low, high) = join.expect("pthread bounds should be available");
        assert!(low < high);
        assert!(high - low > NativeStackBounds::UNWIND_RESERVE);
    }

    #[test]
    fn stack_limit_rejects_a_reserve_that_consumes_the_stack() {
        let bounds = NativeStackBounds {
            low: 100,
            high: 200,
        };
        assert!(bounds.limit_with_frame_reserve(100).is_err());
        assert_eq!(bounds.limit_with_frame_reserve(99), Ok(199 as *const u8));
    }

    #[test]
    fn w5_a1_configured_stack_overflow_records_typed_cause() {
        let machine = MachineState::new();
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null(), no_gc);
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        assert_eq!(
            unsafe { prepared_stack_overflow(&mut vmctx) },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::StackOverflow)
        );
    }
}
