//! Persistent ownership for one prepared compiled program.
//!
//! The machine owns every address that generated code can retain.  A call only
//! borrows it long enough to install its cancellation attachment and temporary
//! result roots; neither attachment survives the native return.

use super::invocation::{OldSpaceScope, RootWords};
use super::run::{
    heap_top_extent, initialize_heap_tops, register_result_roots, runtime_error,
    runtime_error_for_status, runtime_error_from_machine,
    runtime_error_from_machine_or_observation, runtime_error_without_machine, try_root_words,
    try_words,
};
use super::safepoint::NativeStackBounds;
use super::{CompiledProgram, ExecutionError, RunOptions, RunResult, Unsupported};
use crate::context::VMContext;
use crate::host_fns::{gc_trigger, prepared_gc_trigger, RuntimeError};
use crate::machine_state::{MachineDisposition, MachineState};
use crate::old_space::OldSpace;
use crate::prepared_control::CallStatus;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, ValueId};

/// A compiled program and its one persistent mutable prepared-STG substrate.
///
/// This is deliberately !Send: code custody, its VM context, and every live
/// heap root stay on the thread that enters generated code.  A later resident
/// owner may stow the whole machine under its existing single-owner protocol;
/// it must not split these fields into independent registries.
pub struct PreparedMachine {
    program: Rc<CompiledProgram>,
    machine: Rc<MachineState>,
    vmctx: VMContext,
    statics: Arc<StaticRegion>,
    _top_table: RootWords,
    old_space: Box<OldSpace>,
}

struct CancelScope<'a>(&'a MachineState);

impl Drop for CancelScope<'_> {
    fn drop(&mut self) {
        self.0.clear_cancel_flag();
    }
}

struct TemporaryRoots<'a> {
    machine: &'a MachineState,
    mark: usize,
}

impl Drop for TemporaryRoots<'_> {
    fn drop(&mut self) {
        self.machine.truncate_rust_roots(self.mark);
    }
}

impl PreparedMachine {
    /// Install `program` once and retain its mutable heap, static image, top
    /// table, descriptor registry and compiled code until this owner drops.
    pub fn new(program: CompiledProgram, options: &RunOptions) -> Result<Self, ExecutionError> {
        let program = Rc::new(program);
        let statics = Arc::new(program.statics.instantiate()?);
        let top_table = try_root_words(program.top_slots.len())?;
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
        let nursery = try_words(
            options
                .nursery_bytes
                .max(heap_reserve)
                .div_ceil(std::mem::size_of::<u64>()),
        )?;
        let machine = Rc::new(MachineState::new());
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        if let Err(error) = machine.install_prepared_buffer_with_static_region(
            nursery,
            program.descriptors.clone(),
            Some(Arc::clone(&statics)),
        ) {
            machine.clear_stack_map_registry();
            return Err(runtime_error(&machine, error));
        }
        let (start, size) = match machine.gc_active_range() {
            Some(range) => range,
            None => {
                machine.clear_gc_state();
                machine.clear_stack_map_registry();
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
            &program.bytes,
        ) {
            Ok(heap_used) => heap_used,
            Err(cause) => {
                machine.free_session_heap();
                machine.clear_stack_map_registry();
                return Err(runtime_error(&machine, cause));
            }
        };
        let mut vmctx = unsafe { VMContext::new(start, start.add(size), gc_trigger) };
        vmctx.alloc_ptr = unsafe { start.add(heap_used) };
        vmctx.machine_state = Rc::as_ptr(&machine).cast_mut();
        vmctx.prepared_tops = top_table.as_mut_ptr().cast::<usize>().cast_const();

        // Heap tops persist with the machine.  They must not share the
        // run-scoped registry that a call frame truncates on native unwind.
        for spec in &program.heap_top_specs {
            if let Some(&slot) = program.top_slots.get(&spec.id) {
                let root = unsafe { top_table.as_mut_ptr().add(slot).cast::<*mut u8>() };
                machine.register_persistent_root(root);
            }
        }
        Ok(Self {
            program,
            machine,
            vmctx,
            statics,
            _top_table: top_table,
            old_space: Box::new(OldSpace::new()),
        })
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.machine.disposition()
    }

    #[must_use]
    pub fn failure(&self) -> Option<crate::machine_state::MachineFailure> {
        self.machine.last_failure()
    }

    /// Execute one scalar-only entry on the retained machine.
    pub fn run_entry(
        &mut self,
        entry: ValueId,
        arguments: &[u64],
        options: &RunOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunResult, ExecutionError> {
        let (adapter, expected_arguments, has_managed_arguments, result_contract, result_layout) = {
            let compiled = self
                .program
                .entries
                .get(&entry)
                .ok_or(ExecutionError::MissingEntry(entry))?;
            (
                compiled.adapter,
                compiled.abi.physical_arguments().len(),
                compiled
                    .abi
                    .semantic_arguments()
                    .iter()
                    .any(|rep| matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)),
                compiled.abi.semantic_results().clone(),
                compiled.abi.result_layout().clone(),
            )
        };
        if has_managed_arguments {
            return Err(ExecutionError::Unsupported(Unsupported::HostArguments(
                entry,
            )));
        }
        if arguments.len() != expected_arguments {
            return Err(ExecutionError::Arguments {
                expected: expected_arguments,
                actual: arguments.len(),
            });
        }
        let max_native_frame = self.program.pipeline.native_frame_maximum();
        let native_frame_reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(native_frame_reserve)
            .map_err(runtime_error_without_machine)?;
        self.vmctx.prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;

        self.machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        self.machine.set_cancel_flag(cancel);
        let _cancel = CancelScope(&self.machine);
        let mut argument_area = try_words(arguments.len())?;
        argument_area.copy_from_slice(arguments);
        let result_words =
            (result_layout.payload_size() as usize).div_ceil(std::mem::size_of::<u64>());
        let results = try_root_words(result_words.max(1))?;
        let collections_before = self.machine.gc_generation();
        let pointer = self.program.pipeline.get_function_ptr(adapter);
        let raw_status = {
            let _scope = OldSpaceScope::new(&self.machine, &self.old_space)?;
            unsafe {
                let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                    std::mem::transmute(pointer);
                adapter(
                    &mut self.vmctx,
                    results.as_mut_ptr(),
                    argument_area.as_ptr(),
                )
            }
        };
        let status = match CallStatus::from_raw(i64::from(raw_status)) {
            Ok(status) => status,
            Err(_) => {
                self.machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine(&self.machine));
            }
        };
        if status == CallStatus::IntegrityFailure {
            self.machine.set_first_cause(RuntimeError::BadPointer);
        }
        if status != CallStatus::Success
            || self.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(runtime_error_for_status(&self.machine, status));
        }
        if result_contract == ResultContract::NoSuccess {
            return Err(runtime_error(
                &self.machine,
                RuntimeError::NoSuccessReturned,
            ));
        }

        let root_mark = self.machine.rust_roots_len();
        register_result_roots(&self.machine, &results, &result_layout);
        let _roots = TemporaryRoots {
            machine: &self.machine,
            mark: root_mark,
        };
        if options.collect_before_observation {
            Self::collect_on(&self.machine, &mut self.vmctx, &self.old_space, 0)?;
        }
        let result_reps = result_contract
            .returned_reps()
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::NoSuccessReturned))?;
        let _scope = OldSpaceScope::new(&self.machine, &self.old_space)?;
        let result_words = results.snapshot();
        let seeds =
            match super::observe::snapshot_results(&result_words, result_reps, &result_layout) {
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
            &self.program,
            &mut self.vmctx,
            &self.statics,
            &self.program.descriptor_registry,
            &self.old_space,
            &seeds,
            options.observation_budget,
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
        Ok(RunResult {
            values,
            collections: self
                .machine
                .gc_generation()
                .saturating_sub(collections_before),
        })
    }

    fn collect_on(
        machine: &MachineState,
        vmctx: &mut VMContext,
        old_space: &OldSpace,
        reserve: usize,
    ) -> Result<(), ExecutionError> {
        let _scope = OldSpaceScope::new(machine, old_space)?;
        let raw = unsafe { prepared_gc_trigger(vmctx, reserve) };
        let status = CallStatus::from_raw(i64::from(raw))
            .map_err(|_| runtime_error(machine, RuntimeError::BadPointer))?;
        if status != CallStatus::Success || machine.prepared_call_status() != CallStatus::Success {
            return Err(runtime_error_for_status(machine, status));
        }
        Ok(())
    }
}

impl Drop for PreparedMachine {
    fn drop(&mut self) {
        self.machine.clear_prepared_old_space();
        self.machine.clear_rust_roots();
        self.machine.free_session_heap();
        self.machine.clear_stack_map_registry();
        self.machine.clear_cancel_flag();
        self.vmctx.machine_state = std::ptr::null_mut();
        self.vmctx.prepared_tops = std::ptr::null();
    }
}
