//! Exercise generated settlement while both semispaces and the root survive.

use super::{entry_tests::caf_program, safepoint::NativeStackBounds, CompiledProgram, DescriptorMeaning};
use crate::{context::VMContext, host_fns::RuntimeError, machine_state::{MachineState, MachineDisposition}};
use crate::prepared_control::{CallStatus, PreparedSafepoint};
use std::sync::Arc;
use tidepool_heap::execution_descriptor::{DescriptorState, ObjectDescriptor, FORWARDING_POINTER_OFFSET};
use tidepool_heap::managed_reference::untag;
use tidepool_repr::execution_schema::{ValueId, UpdatePolicy};

struct Invocation<'a> {
    program: &'a CompiledProgram,
    machine: Box<MachineState>,
    vmctx: VMContext,
    root: Box<usize>,
    descriptor: Arc<ObjectDescriptor>,
    original: *const u8,
}

impl<'a> Invocation<'a> {
    fn new(program: &'a CompiledProgram) -> Self {
        let descriptor = program.descriptor_registry.values().find_map(|metadata| {
            match metadata.meaning {
                DescriptorMeaning::Callable { binding: ValueId(0), .. } =>
                    Some(Arc::clone(&metadata.descriptor)),
                _ => None,
            }
        }).unwrap();
        let machine = Box::new(MachineState::new());
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        machine.install_prepared_buffer(vec![0_u64; 8], program.descriptors.clone()).unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        unsafe { descriptor.initialize_header(start) };
        let mut root = Box::new(start as usize);
        machine.register_rust_root((&mut *root as *mut usize).cast());
        let mut vmctx = unsafe { VMContext::new(start, start.add(size), crate::host_fns::gc_trigger) };
        vmctx.alloc_ptr = unsafe { start.add(descriptor.allocation_extent() as usize) };
        vmctx.machine_state = (&*machine as *const MachineState).cast_mut();
        vmctx.prepared_stack_limit = NativeStackBounds::current().unwrap()
            .limit_with_frame_reserve(program.pipeline.native_frame_maximum()).unwrap();
        Self { program, machine, vmctx, root, descriptor, original: start }
    }

    fn force(&mut self) -> (CallStatus, usize) {
        NativeStackBounds::current().unwrap().ensure_current_frame_reserve(
            self.program.pipeline.native_frame_maximum() * 2).unwrap();
        let adapter = self.program.pipeline.get_function_ptr(self.program.prepared_force_adapter());
        let adapter: unsafe extern "C" fn(*mut VMContext, *mut u64, usize) -> i32 =
            unsafe { std::mem::transmute(adapter) };
        let mut output = 0xdead_beef_u64;
        let status = unsafe { adapter(&mut self.vmctx, &mut output, *self.root) };
        (CallStatus::from_raw(i64::from(status)).unwrap(), output as usize)
    }

    fn state(&self) -> DescriptorState {
        assert_eq!(self.machine.disposition(), MachineDisposition::Reusable);
        unsafe { self.descriptor.state(*self.root as *const u8,
            self.descriptor.allocation_extent() as usize) }.unwrap()
    }
}

impl Drop for Invocation<'_> {
    fn drop(&mut self) {
        self.machine.clear_gc_state();
        self.machine.clear_stack_map_registry();
    }
}

#[test]
fn w5_a1_update_lands_on_relocated_thunk_and_source_is_forwarded() {
    let program = caf_program(3, false, UpdatePolicy::Memoize);
    let mut invocation = Invocation::new(&program);
    let (status, result) = invocation.force();
    assert_eq!(status, CallStatus::Success);
    assert_eq!(invocation.machine.gc_generation(), 1, "keep the source semispace owned and unreused");
    assert_ne!(*invocation.root, invocation.original as usize);
    assert_eq!(invocation.state(), DescriptorState::Updated);
    let source_state = unsafe { invocation.descriptor.state(invocation.original,
        invocation.descriptor.allocation_extent() as usize) }.unwrap();
    assert_eq!(source_state, DescriptorState::Forwarded);
    let target = unsafe { ((*invocation.root + FORWARDING_POINTER_OFFSET) as *const usize).read() };
    assert_eq!(target, result);
    assert_ne!(untag(result), 0);
}

#[test]
fn w5_a1_cancellation_settles_live_and_does_not_publish_output() {
    for policy in [UpdatePolicy::Memoize, UpdatePolicy::SingleEntry] {
    for (point, occurrence) in [
        (PreparedSafepoint::FunctionEntry, 1),
        (PreparedSafepoint::ThunkEntry, 2),
        (PreparedSafepoint::Allocation, 1),
        (PreparedSafepoint::ThunkCommit, 1),
    ] {
        let program = caf_program(3, false, policy);
        let mut invocation = Invocation::new(&program);
        invocation.machine.fail_prepared_at(point, occurrence, RuntimeError::Cancelled);
        let (status, output) = invocation.force();
        assert_eq!(status, CallStatus::Cancelled, "{point:?}");
        assert_eq!(output, 0xdead_beef, "failed force must not publish a payload");
        assert_eq!(invocation.state(), DescriptorState::Live, "{point:?}");
        assert_eq!(invocation.machine.take_runtime_error(), Some(RuntimeError::Cancelled));
        assert_eq!(invocation.force().0, CallStatus::Success,
            "a settled reusable thunk must permit retry: {point:?} {policy:?}");
    }
    }
}
