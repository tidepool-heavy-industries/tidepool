use super::{CompiledProgram, ObservationFailure, Unsupported};
use crate::context::VMContext;
use crate::host_fns::{gc_trigger, prepared_gc_trigger, RuntimeError};
use crate::machine_state::MachineFailure;
use crate::machine_state::{MachineDisposition, MachineState};
use crate::prepared_control::CallStatus;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_bridge::Value;
use tidepool_repr::execution_schema::ValueId;

pub struct RunOptions {
    pub nursery_bytes: usize,
    pub observation_budget: usize,
    /// Contract-test/diagnostic request, using the ordinary collector after
    /// native unwind and result-root admission, before nonforcing observation.
    pub collect_before_observation: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            nursery_bytes: 4096,
            observation_budget: 100_000,
            collect_before_observation: false,
        }
    }
}

#[derive(Debug)]
pub struct RunResult {
    pub values: Vec<Value>,
    pub collections: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("entry {0:?} is not exported by this program")]
    MissingEntry(ValueId),
    #[error("entry arguments: expected {expected} physical scalar slots, got {actual}")]
    Arguments { expected: usize, actual: usize },
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    #[error("{cause}", cause = .0.cause)]
    Runtime(MachineFailure),
    #[error(transparent)]
    Observation(#[from] ObservationFailure),
    #[error(transparent)]
    Static(#[from] tidepool_heap::static_region::StaticImageError),
}

impl CompiledProgram {
    pub fn run_entry(
        &self,
        entry: ValueId,
        arguments: &[u64],
        options: &RunOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunResult, ExecutionError> {
        // wave4:INVOCATION — scalar-only host admission, private static
        // instantiation/top table, fresh MachineState with code/maps/descriptors
        // pinned through unwind, ordinary prepared nursery, vector C adapter.
        // Check status/first cause BEFORE registering or reading result slots.
        // Register managed result layout slots before optional collection;
        // restore root mark on every exit. Observe while storage remains owned.
        // Integrity in observation retires machine with first cause retained.
        // No compiled address or returned Value may retain an invocation pointer.
        let compiled = self
            .entries
            .get(&entry)
            .ok_or(ExecutionError::MissingEntry(entry))?;
        if compiled.abi.semantic_arguments().iter().any(|rep| {
            matches!(
                rep,
                tidepool_repr::execution_schema::RuntimeRep::LiftedRef
                    | tidepool_repr::execution_schema::RuntimeRep::UnliftedRef
            )
        }) {
            return Err(ExecutionError::Unsupported(Unsupported::HostArguments(
                entry,
            )));
        }
        if arguments.len() != compiled.abi.physical_arguments().len() {
            return Err(ExecutionError::Arguments {
                expected: compiled.abi.physical_arguments().len(),
                actual: arguments.len(),
            });
        }

        let statics = Arc::new(self.statics.instantiate()?);
        let mut top_table = try_slots(self.top_slots.len())?;
        for (&id, &slot) in &self.top_slots {
            let value = statics
                .entry(id)
                .or_else(|| self.byte_tops.get(&id).map(|bytes| bytes.as_ptr() as usize))
                .ok_or(ExecutionError::MissingEntry(id))?;
            let slot = top_table
                .get_mut(slot)
                .ok_or(ExecutionError::MissingEntry(id))?;
            *slot = value;
        }

        let nursery = try_words(options.nursery_bytes.div_ceil(std::mem::size_of::<u64>()))?;
        let mut argument_area = try_words(arguments.len())?;
        argument_area.copy_from_slice(arguments);
        let result_words = (compiled.abi.result_layout().payload_size() as usize)
            .div_ceil(std::mem::size_of::<u64>());
        let mut result_area = try_words(result_words.max(1))?;

        let machine = MachineState::new();
        machine.set_cancel_flag(Arc::clone(&cancel));
        machine.set_stack_map_registry(&self.pipeline.stack_maps);
        if let Err(error) = machine.install_prepared_buffer_with_static_region(
            nursery,
            self.descriptors.clone(),
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
        let mut vmctx = unsafe { VMContext::new(start, start.add(size), gc_trigger) };
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        vmctx.prepared_tops = top_table.as_ptr();
        let root_mark = machine.rust_roots_len();
        let mut cleanup = RunCleanup::new(&machine, &mut vmctx, root_mark);
        let collections_before = machine.gc_generation();

        let result = (|| {
            let pointer = self.pipeline.get_function_ptr(compiled.adapter);
            let raw_status = unsafe {
                let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                    std::mem::transmute(pointer);
                adapter(&mut vmctx, result_area.as_mut_ptr(), argument_area.as_ptr())
            };
            let status = match CallStatus::from_raw(i64::from(raw_status)) {
                Ok(status) => status,
                Err(_) => {
                    machine.set_first_cause(RuntimeError::BadPointer);
                    return Err(runtime_error_from_machine(&machine));
                }
            };
            if status == CallStatus::IntegrityFailure {
                machine.set_first_cause(RuntimeError::BadPointer);
            }
            if status != CallStatus::Success
                || machine.prepared_call_status() != CallStatus::Success
            {
                return Err(runtime_error_for_status(&machine, status));
            }

            register_result_roots(&machine, &mut result_area, compiled.abi.result_layout());
            if options.collect_before_observation {
                let raw_status = unsafe { prepared_gc_trigger(&mut vmctx, 0) };
                let status = match CallStatus::from_raw(i64::from(raw_status)) {
                    Ok(status) => status,
                    Err(_) => {
                        machine.set_first_cause(RuntimeError::BadPointer);
                        CallStatus::IntegrityFailure
                    }
                };
                if status != CallStatus::Success
                    || machine.prepared_call_status() != CallStatus::Success
                {
                    return Err(runtime_error_for_status(&machine, status));
                }
            }

            let (active_start, active_size) = machine
                .gc_active_range()
                .ok_or_else(|| runtime_error(&machine, RuntimeError::BadPointer))?;
            let cursor = (vmctx.alloc_ptr as usize)
                .checked_sub(active_start as usize)
                .filter(|cursor| {
                    *cursor <= active_size && *cursor % std::mem::size_of::<u64>() == 0
                })
                .ok_or_else(|| runtime_error(&machine, RuntimeError::BadPointer))?;
            let (buffer, used) = machine.reclaim_session_heap(vmctx.alloc_ptr);
            if used != cursor || used > active_size || used % std::mem::size_of::<u64>() != 0 {
                return Err(runtime_error(&machine, RuntimeError::BadPointer));
            }
            let buffer = buffer.ok_or_else(|| runtime_error(&machine, RuntimeError::BadPointer))?;
            let buffer_bytes = buffer
                .len()
                .checked_mul(std::mem::size_of::<u64>())
                .ok_or_else(|| runtime_error(&machine, RuntimeError::BadPointer))?;
            if used > buffer_bytes {
                return Err(runtime_error(&machine, RuntimeError::BadPointer));
            }
            let used_words = used.div_ceil(std::mem::size_of::<u64>());
            let nursery = &buffer[..used_words];
            let heap = match super::observe::ObservationHeap::new(
                nursery,
                &statics,
                self.descriptors.clone(),
                &self.constructors,
            ) {
                Ok(heap) => heap,
                Err(error @ ObservationFailure::Integrity(_)) => {
                    machine.set_first_cause(RuntimeError::BadPointer);
                    return Err(runtime_error_from_machine_or_observation(&machine, error));
                }
                Err(error) => return Err(error.into()),
            };
            let values = match heap.observe_results(
                &result_area,
                compiled.abi.semantic_results(),
                compiled.abi.result_layout(),
                options.observation_budget,
            ) {
                Ok(values) => values,
                Err(error @ ObservationFailure::Integrity(_)) => {
                    machine.set_first_cause(RuntimeError::BadPointer);
                    return Err(runtime_error_from_machine_or_observation(&machine, error));
                }
                Err(error) => return Err(error.into()),
            };
            cleanup.finish();
            Ok(RunResult {
                values,
                collections: machine.gc_generation().saturating_sub(collections_before),
            })
        })();
        cleanup.drop_now();
        machine.clear_cancel_flag();
        result
    }
}

fn try_words(words: usize) -> Result<Vec<u64>, ExecutionError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(words)
        .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
    result.resize(words, 0);
    Ok(result)
}

fn try_slots(slots: usize) -> Result<Vec<usize>, ExecutionError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(slots)
        .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
    result.resize(slots, 0);
    Ok(result)
}

fn register_result_roots(
    machine: &MachineState,
    result_area: &mut [u64],
    layout: &tidepool_repr::execution_schema::StorageLayout,
) {
    for field in layout.fields() {
        if matches!(
            field.rep(),
            tidepool_repr::execution_schema::RuntimeRep::LiftedRef
                | tidepool_repr::execution_schema::RuntimeRep::UnliftedRef
        ) {
            let slot = unsafe {
                result_area
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(field.offset() as usize)
                    .cast::<*mut u8>()
            };
            machine.register_rust_root(slot);
        }
    }
}

struct RunCleanup<'a> {
    machine: &'a MachineState,
    vmctx: *mut VMContext,
    root_mark: usize,
    armed: bool,
}

impl<'a> RunCleanup<'a> {
    fn new(machine: &'a MachineState, vmctx: &mut VMContext, root_mark: usize) -> Self {
        Self {
            machine,
            vmctx,
            root_mark,
            armed: true,
        }
    }

    fn finish(&mut self) {
        if !self.armed {
            return;
        }
        self.machine.truncate_rust_roots(self.root_mark);
        let _ = self
            .machine
            .reclaim_session_heap(unsafe { (*self.vmctx).alloc_ptr });
        self.machine.clear_gc_state();
        self.machine.clear_stack_map_registry();
        self.machine.clear_cancel_flag();
        self.armed = false;
    }

    fn drop_now(&mut self) {
        self.finish();
    }
}

impl Drop for RunCleanup<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

fn runtime_error(machine: &MachineState, error: RuntimeError) -> ExecutionError {
    machine.set_first_cause(error);
    runtime_error_from_machine(machine)
}

fn runtime_error_from_machine(machine: &MachineState) -> ExecutionError {
    ExecutionError::Runtime(machine.last_failure().unwrap_or(MachineFailure {
        cause: RuntimeError::BadPointer,
        disposition: MachineDisposition::Unavailable,
    }))
}

fn runtime_error_for_status(machine: &MachineState, status: CallStatus) -> ExecutionError {
    if machine.last_failure().is_none() {
        machine.set_first_cause(match status {
            CallStatus::Cancelled => RuntimeError::Cancelled,
            CallStatus::LanguageFailure => RuntimeError::BadPointer,
            CallStatus::IntegrityFailure | CallStatus::Success => RuntimeError::BadPointer,
        });
    }
    runtime_error_from_machine(machine)
}

fn runtime_error_from_machine_or_observation(
    machine: &MachineState,
    observation: ObservationFailure,
) -> ExecutionError {
    if machine.last_failure().is_some() {
        runtime_error_from_machine(machine)
    } else {
        ExecutionError::Observation(observation)
    }
}

fn runtime_error_without_machine(error: RuntimeError) -> ExecutionError {
    ExecutionError::Runtime(MachineFailure {
        disposition: error.machine_disposition(),
        cause: error,
    })
}
