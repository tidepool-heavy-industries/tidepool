//! One invocation owns every mutable address reached by its generated code.
//!
//! Wave5:A5_INVOCATION. Implement `enter(program, entry, arguments, options,
//! cancel) -> Result<Self, ExecutionError>` and `observe(&mut self, budget) ->
//! Result<RunResult, ExecutionError>` by moving the existing run_entry path,
//! not by maintaining a second execution wrapper. `run_entry` uses both.
//! Enter admits arguments, installs roots, executes and checks status before
//! publishing rooted results. Observe may collect and must re-read root slots.
//! The subsequent retention parcel adds promote_result(logical_index) here;
//! no independent RootSlot or native pointer may escape this owner.
//!
//! Keep the compiled borrow and private OldSpace together: OldSpace's legacy
//! unsafe Send must not be used to transfer !Send code custody. This owner is
//! intentionally !Send via its compiled-program borrow. A stable Box holds
//! MachineState because VMContext embeds its address. There is no self-borrowed
//! cleanup guard: Drop removes registries before releasing their allocations.

use super::run::{
    heap_top_extent, initialize_heap_tops, runtime_error, runtime_error_for_status,
    runtime_error_from_machine, runtime_error_from_machine_or_observation,
    runtime_error_without_machine, try_words,
};
use super::safepoint::NativeStackBounds;
use super::{CompiledProgram, ExecutionError};
use crate::host_fns::{gc_trigger, prepared_gc_trigger, RuntimeError};
use crate::prepared_control::{CallStatus, PreparedSafepoint};
use crate::{context::VMContext, machine_state::MachineState, old_space::OldSpace};
use std::cell::UnsafeCell;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{RuntimeRep, StorageLayout, ValueId};

pub(super) struct PreparedInvocation<'code> {
    pub(super) program: &'code CompiledProgram,
    pub(super) machine: Box<MachineState>,
    pub(super) vmctx: VMContext,
    pub(super) statics: Arc<StaticRegion>,
    pub(super) top_table: RootWords,
    pub(super) results: RootWords,
    pub(super) result_reps: Vec<RuntimeRep>,
    pub(super) result_layout: StorageLayout,
    pub(super) collections_before: u64,
    old_space: OldSpace,
}

/// Initialized, fixed-address storage with explicit collector interior writes.
/// Never expose a shared slice into it across a generated call. Scalar snapshots
/// are owned values; registered raw slots live until invocation teardown.
pub(super) struct RootWords(Box<[UnsafeCell<u64>]>);

impl RootWords {
    pub(super) fn new(length: usize) -> Result<Self, ExecutionError> {
        let mut words = Vec::new();
        words.try_reserve_exact(length).map_err(|_| {
            super::run::runtime_error_without_machine(crate::host_fns::RuntimeError::HeapOverflow)
        })?;
        words.resize_with(length, || UnsafeCell::new(0));
        Ok(Self(words.into_boxed_slice()))
    }

    pub(super) fn as_mut_ptr(&self) -> *mut u64 {
        self.0.as_ptr().cast::<u64>().cast_mut()
    }

    pub(super) fn write(&self, index: usize, value: u64) -> Result<(), ExecutionError> {
        let word = self
            .0
            .get(index)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::BadPointer))?;
        unsafe { word.get().write(value) };
        Ok(())
    }

    pub(super) fn snapshot(&self) -> Vec<u64> {
        self.0.iter().map(|word| unsafe { *word.get() }).collect()
    }
}

impl<'code> PreparedInvocation<'code> {
    pub(super) fn enter(
        program: &'code CompiledProgram,
        entry: ValueId,
        arguments: &[u64],
        options: &super::run::RunOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, ExecutionError> {
        Self::enter_with_poll_failure(program, entry, arguments, options, cancel, None)
    }

    #[cfg(test)]
    pub(super) fn enter_with_injected_poll_failure(
        program: &'code CompiledProgram,
        entry: ValueId,
        arguments: &[u64],
        options: &super::run::RunOptions,
        cancel: Arc<AtomicBool>,
        failure: (PreparedSafepoint, usize, RuntimeError),
    ) -> Result<Self, ExecutionError> {
        Self::enter_with_poll_failure(program, entry, arguments, options, cancel, Some(failure))
    }

    fn enter_with_poll_failure(
        program: &'code CompiledProgram,
        entry: ValueId,
        arguments: &[u64],
        options: &super::run::RunOptions,
        cancel: Arc<AtomicBool>,
        failure: Option<(PreparedSafepoint, usize, RuntimeError)>,
    ) -> Result<Self, ExecutionError> {
        let compiled = program
            .entries
            .get(&entry)
            .ok_or(ExecutionError::MissingEntry(entry))?;
        if compiled
            .abi
            .semantic_arguments()
            .iter()
            .any(|rep| matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef))
        {
            return Err(ExecutionError::Unsupported(
                super::Unsupported::HostArguments(entry),
            ));
        }
        if arguments.len() != compiled.abi.physical_arguments().len() {
            return Err(ExecutionError::Arguments {
                expected: compiled.abi.physical_arguments().len(),
                actual: arguments.len(),
            });
        }

        let max_native_frame = program.pipeline.native_frame_maximum();
        let native_frame_reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(native_frame_reserve)
            .map_err(runtime_error_without_machine)?;
        let prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;

        let statics = Arc::new(program.statics.instantiate()?);
        let top_table = super::run::try_root_words(program.top_slots.len())?;
        for (&id, &slot) in &program.top_slots {
            if program.heap_top_specs.iter().any(|spec| spec.id == id) {
                continue;
            }
            let value = statics
                .entry(id)
                .or_else(|| {
                    program
                        .byte_tops
                        .get(&id)
                        .map(|bytes| bytes.as_ptr() as usize)
                })
                .ok_or(ExecutionError::MissingEntry(id))?;
            top_table.write(slot, value as u64)?;
        }

        let heap_reserve = heap_top_extent(&program.heap_top_specs)?;
        let nursery_bytes = options.nursery_bytes.max(heap_reserve);
        let nursery = try_words(nursery_bytes.div_ceil(std::mem::size_of::<u64>()))?;
        let mut argument_area = try_words(arguments.len())?;
        argument_area.copy_from_slice(arguments);
        let result_words = (compiled.abi.result_layout().payload_size() as usize)
            .div_ceil(std::mem::size_of::<u64>());
        let results = super::run::try_root_words(result_words.max(1))?;

        let machine = Box::new(MachineState::new());
        machine.set_cancel_flag(Arc::clone(&cancel));
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        if let Err(error) = machine.install_prepared_buffer_with_static_region(
            nursery,
            program.descriptors.clone(),
            Some(Arc::clone(&statics)),
        ) {
            machine.clear_stack_map_registry();
            machine.clear_cancel_flag();
            return Err(runtime_error(&machine, error));
        }
        let (start, size) = match machine.gc_active_range() {
            Some(range) => range,
            None => {
                machine.clear_gc_state();
                machine.clear_stack_map_registry();
                machine.clear_cancel_flag();
                return Err(runtime_error(&machine, RuntimeError::BadPointer));
            }
        };
        let heap_used = match initialize_heap_tops(
            start,
            size,
            &program.heap_top_specs,
            &program.top_slots,
            &top_table,
            &statics,
            &program.byte_tops,
        ) {
            Ok(heap_used) => heap_used,
            Err(cause) => {
                machine.clear_rust_roots();
                machine.free_session_heap();
                machine.clear_stack_map_registry();
                machine.clear_cancel_flag();
                return Err(runtime_error(&machine, cause));
            }
        };
        let mut vmctx = unsafe { VMContext::new(start, start.add(size), gc_trigger) };
        vmctx.alloc_ptr = unsafe { start.add(heap_used) };
        vmctx.machine_state = (&*machine as *const MachineState).cast_mut();
        vmctx.prepared_tops = top_table.as_mut_ptr().cast::<usize>().cast_const();
        vmctx.prepared_stack_limit = prepared_stack_limit;

        let mut invocation = Self {
            program,
            machine,
            vmctx,
            statics,
            top_table,
            results,
            result_reps: compiled.abi.semantic_results().to_vec(),
            result_layout: compiled.abi.result_layout().clone(),
            collections_before: 0,
            old_space: OldSpace::new(),
        };
        for spec in &invocation.program.heap_top_specs {
            if let Some(&slot) = invocation.program.top_slots.get(&spec.id) {
                let root = unsafe {
                    invocation
                        .top_table
                        .as_mut_ptr()
                        .add(slot)
                        .cast::<*mut u8>()
                };
                invocation.machine.register_rust_root(root);
            }
        }
        invocation.collections_before = invocation.machine.gc_generation();
        #[cfg(test)]
        if let Some((point, occurrence, cause)) = failure {
            invocation
                .machine
                .fail_prepared_at(point, occurrence, cause);
        }
        #[cfg(not(test))]
        let _ = failure;

        let pointer = invocation
            .program
            .pipeline
            .get_function_ptr(compiled.adapter);
        let raw_status = unsafe {
            let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                std::mem::transmute(pointer);
            adapter(
                &mut invocation.vmctx,
                invocation.results.as_mut_ptr(),
                argument_area.as_ptr(),
            )
        };
        let status = match CallStatus::from_raw(i64::from(raw_status)) {
            Ok(status) => status,
            Err(_) => {
                invocation.machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine(&invocation.machine));
            }
        };
        if status == CallStatus::IntegrityFailure {
            invocation.machine.set_first_cause(RuntimeError::BadPointer);
        }
        if status != CallStatus::Success
            || invocation.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(runtime_error_for_status(&invocation.machine, status));
        }

        super::run::register_result_roots(
            &invocation.machine,
            &invocation.results,
            &invocation.result_layout,
        );
        if options.collect_before_observation {
            let raw_status = unsafe { prepared_gc_trigger(&mut invocation.vmctx, 0) };
            let status = match CallStatus::from_raw(i64::from(raw_status)) {
                Ok(status) => status,
                Err(_) => {
                    invocation.machine.set_first_cause(RuntimeError::BadPointer);
                    CallStatus::IntegrityFailure
                }
            };
            if status != CallStatus::Success
                || invocation.machine.prepared_call_status() != CallStatus::Success
            {
                return Err(runtime_error_for_status(&invocation.machine, status));
            }
        }
        Ok(invocation)
    }

    pub(super) fn observe(
        &mut self,
        budget: usize,
    ) -> Result<super::run::RunResult, ExecutionError> {
        if self.machine.prepared_call_status() != CallStatus::Success {
            return Err(runtime_error_from_machine(&self.machine));
        }
        let result_words = self.results.snapshot();
        let result_seeds = match super::observe::snapshot_results(
            &result_words,
            &self.result_reps,
            &self.result_layout,
        ) {
            Ok(seeds) => seeds,
            Err(error @ super::ObservationFailure::Integrity(_)) => {
                self.machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine_or_observation(
                    &self.machine,
                    error,
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let values = match super::forcing::observe_results(
            &self.machine,
            self.program,
            &mut self.vmctx,
            &self.statics,
            &self.program.descriptor_registry,
            &result_seeds,
            budget,
        ) {
            Ok(values) => values,
            Err(ExecutionError::Observation(error @ super::ObservationFailure::Integrity(_))) => {
                self.machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine_or_observation(
                    &self.machine,
                    error,
                ));
            }
            Err(error) => return Err(error),
        };
        Ok(super::run::RunResult {
            values,
            collections: self
                .machine
                .gc_generation()
                .saturating_sub(self.collections_before),
        })
    }
}

impl Drop for PreparedInvocation<'_> {
    fn drop(&mut self) {
        // Native execution has returned before this owner can be dropped.
        // Teardown consults ownership tables only, even after terminal failure.
        self.machine.clear_rust_roots();
        self.machine.free_session_heap();
        self.machine.clear_stack_map_registry();
        self.machine.clear_cancel_flag();
        self.vmctx.machine_state = std::ptr::null_mut();
        self.vmctx.prepared_tops = std::ptr::null();
    }
}
