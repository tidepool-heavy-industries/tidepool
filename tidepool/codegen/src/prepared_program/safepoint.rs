//! Prepared safepoints use the invocation machine, never legacy TLS state.

use crate::context::VMContext;
use crate::host_fns::RuntimeError;
use crate::machine_state::machine_state;

/// Return the current ABI status after sampling cancellation. Callers branch
/// before reading payloads or mutating the heap. The VMContext owns the machine
/// association even when another legacy machine is installed on the thread.
///
/// # Safety
/// `vmctx` and its machine must remain valid throughout the call.
/// Typed generated poll boundary. Unknown discriminants are integrity failures,
/// never permission to skip cancellation. The emitter supplies constants.
pub(super) unsafe extern "C" fn prepared_poll_at(vmctx: *mut VMContext, point: u32) -> i32 {
    let machine = unsafe { machine_state(vmctx) };
    match crate::prepared_control::PreparedSafepoint::from_raw(point) {
        Some(point) => machine.poll_prepared(point) as i32,
        None => {
            machine.set_first_cause(crate::host_fns::bad_pointer());
            machine.prepared_call_status() as i32
        }
    }
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

    #[test]
    fn w5_a1_allocation_cancel_records_on_invocation_without_tls() {
        let machine = MachineState::new();
        machine.set_cancel_flag(Arc::new(AtomicBool::new(true)));
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null());
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        let status = unsafe { crate::host_fns::prepared_gc_trigger(&mut vmctx, 8) };
        assert_eq!(status, CallStatus::Cancelled as i32);
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }

    #[test]
    fn w5_a1_injected_failure_is_scoped_to_named_poll() {
        use crate::prepared_control::PreparedSafepoint;
        let machine = MachineState::new();
        machine.fail_prepared_at(PreparedSafepoint::ThunkCommit, 2, RuntimeError::Cancelled);
        assert_eq!(
            machine.poll_prepared(PreparedSafepoint::Backedge),
            CallStatus::Success
        );
        assert_eq!(
            machine.poll_prepared(PreparedSafepoint::ThunkCommit),
            CallStatus::Success
        );
        assert_eq!(
            machine.poll_prepared(PreparedSafepoint::ThunkCommit),
            CallStatus::Cancelled
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }

    #[test]
    fn w5_a1_poll_records_on_invocation_without_tls() {
        let machine = MachineState::new();
        machine.set_cancel_flag(Arc::new(AtomicBool::new(true)));
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null());
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        let status = unsafe {
            prepared_poll_at(
                &mut vmctx,
                crate::prepared_control::PreparedSafepoint::FunctionEntry as u32,
            )
        };
        assert_ne!(status, CallStatus::Success as i32);
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }

    #[test]
    fn w5_a1_poll_preserves_terminal_first_cause() {
        let machine = MachineState::new();
        machine.set_first_cause(crate::host_fns::bad_pointer());
        machine.set_cancel_flag(Arc::new(AtomicBool::new(true)));
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null());
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        assert_eq!(
            unsafe {
                prepared_poll_at(
                    &mut vmctx,
                    crate::prepared_control::PreparedSafepoint::FunctionEntry as u32,
                )
            },
            CallStatus::IntegrityFailure as i32
        );
        assert!(matches!(
            machine.take_runtime_error(),
            Some(RuntimeError::BadPointer { .. })
        ));
    }

    #[test]
    fn w5_a1_configured_stack_overflow_records_typed_cause() {
        let machine = MachineState::new();
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null());
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        assert_eq!(
            unsafe { prepared_stack_overflow(&mut vmctx) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::StackOverflow)
        );
    }
}
