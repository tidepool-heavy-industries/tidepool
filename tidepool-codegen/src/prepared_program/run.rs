use super::plan::HeapTopSpec;
use super::safepoint::NativeStackBounds;
use super::{CompiledProgram, ObservationFailure, Unsupported};
use crate::context::VMContext;
use crate::host_fns::{gc_trigger, prepared_gc_trigger, RuntimeError};
use crate::machine_state::MachineFailure;
use crate::machine_state::{MachineDisposition, MachineState};
use crate::prepared_control::CallStatus;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_bridge::Value;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::ValueId;
use tidepool_repr::execution_schema::{Atom, HeapRhs, RuntimeRep, ValueRef};

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

        // The adapter and its first generated callee can both consume a
        // finalized native frame before another generated entry preflight.
        // The OS helper already places the guard/unwind reserve below `low`;
        // reserve two complete compiled frames above it for this initial hop.
        let max_native_frame = self.pipeline.native_frame_maximum();
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

        let statics = Arc::new(self.statics.instantiate()?);
        let mut top_table = try_slots(self.top_slots.len())?;
        for (&id, &slot) in &self.top_slots {
            if self.heap_top_specs.iter().any(|spec| spec.id == id) {
                continue;
            }
            let value = statics
                .entry(id)
                .or_else(|| self.byte_tops.get(&id).map(|bytes| bytes.as_ptr() as usize))
                .ok_or(ExecutionError::MissingEntry(id))?;
            let slot = top_table
                .get_mut(slot)
                .ok_or(ExecutionError::MissingEntry(id))?;
            *slot = value;
        }

        let heap_reserve = heap_top_extent(&self.heap_top_specs)?;
        let nursery_bytes = options.nursery_bytes.max(heap_reserve);
        let nursery = try_words(nursery_bytes.div_ceil(std::mem::size_of::<u64>()))?;
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
        let heap_used = initialize_heap_tops(
            start,
            size,
            &self.heap_top_specs,
            &self.top_slots,
            &mut top_table,
            &statics,
            &self.byte_tops,
        )
        .map_err(|cause| runtime_error(&machine, cause))?;
        let mut vmctx = unsafe { VMContext::new(start, start.add(size), gc_trigger) };
        vmctx.alloc_ptr = unsafe { start.add(heap_used) };
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        vmctx.prepared_tops = top_table.as_ptr();
        vmctx.prepared_stack_limit = prepared_stack_limit;
        for spec in &self.heap_top_specs {
            if let Some(&slot) = self.top_slots.get(&spec.id) {
                let root = unsafe { top_table.as_mut_ptr().add(slot).cast::<*mut u8>() };
                machine.register_rust_root(root);
            }
        }
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

            // Forcing observation must run before reclaiming the invocation
            // buffer: a child thunk may allocate and collect, and every
            // borrowed nursery view is dropped before that force begins.
            // Copy physical result words before any generated force. The
            // forcing observer may collect and rewrite registered result
            // slots; no shared Rust slice may remain live across that call.
            let result_seeds = match super::observe::snapshot_results(
                &result_area,
                compiled.abi.semantic_results(),
                compiled.abi.result_layout(),
            ) {
                Ok(seeds) => seeds,
                Err(error @ ObservationFailure::Integrity(_)) => {
                    machine.set_first_cause(RuntimeError::BadPointer);
                    return Err(runtime_error_from_machine_or_observation(&machine, error));
                }
                Err(error) => return Err(error.into()),
            };
            let values = match super::forcing::observe_results(
                &machine,
                self,
                &mut vmctx,
                &statics,
                &self.descriptor_registry,
                &result_seeds,
                options.observation_budget,
            ) {
                Ok(values) => values,
                Err(ExecutionError::Observation(error @ ObservationFailure::Integrity(_))) => {
                    machine.set_first_cause(RuntimeError::BadPointer);
                    return Err(runtime_error_from_machine_or_observation(&machine, error));
                }
                Err(error) => return Err(error),
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

fn heap_top_extent(specs: &[HeapTopSpec]) -> Result<usize, ExecutionError> {
    specs.iter().try_fold(0usize, |total, spec| {
        total
            .checked_add(spec.descriptor.allocation_extent() as usize)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::HeapOverflow))
    })
}

fn initialize_heap_tops(
    start: *mut u8,
    capacity: usize,
    specs: &[HeapTopSpec],
    top_slots: &std::collections::BTreeMap<ValueId, usize>,
    top_table: &mut [usize],
    statics: &tidepool_heap::static_region::StaticRegion,
    byte_tops: &std::collections::BTreeMap<ValueId, Arc<[u8]>>,
) -> Result<usize, RuntimeError> {
    let mut offsets = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for spec in specs {
        offsets.insert(spec.id, total);
        total = total
            .checked_add(spec.descriptor.allocation_extent() as usize)
            .ok_or(RuntimeError::HeapOverflow)?;
    }
    if total > capacity || total % 8 != 0 {
        return Err(RuntimeError::HeapOverflow);
    }
    let base = start as usize;
    let descriptors = specs
        .iter()
        .map(|spec| (spec.id, &spec.descriptor))
        .collect::<std::collections::BTreeMap<_, _>>();
    let pointer = |id: ValueId| -> Result<usize, RuntimeError> {
        if let Some(offset) = offsets.get(&id) {
            let descriptor = descriptors.get(&id).ok_or(RuntimeError::BadPointer)?;
            return base
                .checked_add(*offset)
                .and_then(|address| address.checked_add(usize::from(descriptor.tag())))
                .ok_or(RuntimeError::BadPointer);
        }
        statics.entry(id).ok_or(RuntimeError::BadPointer)
    };
    for spec in specs {
        let offset = offsets[&spec.id];
        let object = unsafe { start.add(offset) };
        unsafe { spec.descriptor.initialize_header(object) };
        match &spec.binding.rhs {
            HeapRhs::Constructor { fields, .. } => {
                write_atoms(
                    object,
                    &spec.descriptor,
                    fields,
                    &spec.reps,
                    &pointer,
                    byte_tops,
                )?;
            }
            HeapRhs::Function { captures, .. } | HeapRhs::Thunk { captures, .. } => {
                let atoms = captures.iter().cloned().map(Atom::Ref).collect::<Vec<_>>();
                write_atoms(
                    object,
                    &spec.descriptor,
                    &atoms,
                    &spec.reps,
                    &pointer,
                    byte_tops,
                )?;
            }
            HeapRhs::Bytes(_) => return Err(RuntimeError::BadPointer),
        }
        if let Some(&slot) = top_slots.get(&spec.id) {
            top_table[slot] = pointer(spec.id)?;
        }
    }
    Ok(total)
}

fn write_atoms(
    object: *mut u8,
    descriptor: &ObjectDescriptor,
    atoms: &[Atom],
    reps: &[RuntimeRep],
    pointer: &impl Fn(ValueId) -> Result<usize, RuntimeError>,
    byte_tops: &std::collections::BTreeMap<ValueId, Arc<[u8]>>,
) -> Result<(), RuntimeError> {
    for (logical, (atom, rep)) in atoms.iter().zip(reps).enumerate() {
        let Some(stored) = descriptor
            .payload()
            .logical_to_stored()
            .get(logical)
            .and_then(|slot| *slot)
        else {
            continue;
        };
        if *rep == RuntimeRep::Void {
            continue;
        }
        let field = &descriptor.payload().fields()[stored as usize];
        let address = descriptor.payload_base() as usize + field.offset() as usize;
        let value = match (rep, atom) {
            (RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef, Atom::Ref(ValueRef::Local(id))) => {
                let value = pointer(*id)?;
                if field.size() as usize != std::mem::size_of::<usize>() {
                    return Err(RuntimeError::BadPointer);
                }
                value.to_ne_bytes().to_vec()
            }
            (RuntimeRep::Address, Atom::Ref(ValueRef::Local(id))) => byte_tops
                .get(id)
                .map(|bytes| bytes.as_ptr() as usize)
                .ok_or(RuntimeError::BadPointer)?
                .to_ne_bytes()
                .to_vec(),
            (RuntimeRep::Address, Atom::Scalar(literal)) => match literal {
                tidepool_repr::execution_schema::ScalarLiteral::NullAddress => {
                    vec![0; field.size() as usize]
                }
                tidepool_repr::execution_schema::ScalarLiteral::Bytes(bytes) => byte_tops
                    .values()
                    .find(|candidate| candidate.as_ref() == bytes.as_slice())
                    .map(|bytes| bytes.as_ptr() as usize)
                    .ok_or(RuntimeError::BadPointer)?
                    .to_ne_bytes()
                    .to_vec(),
                _ => return Err(RuntimeError::BadPointer),
            },
            (
                RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_),
                Atom::Scalar(literal),
            ) => {
                let bytes = match literal {
                    tidepool_repr::execution_schema::ScalarLiteral::Int { bytes, .. }
                    | tidepool_repr::execution_schema::ScalarLiteral::Word { bytes, .. }
                    | tidepool_repr::execution_schema::ScalarLiteral::Float { bytes, .. } => bytes,
                    _ => return Err(RuntimeError::BadPointer),
                };
                if bytes.len() != field.size() as usize {
                    return Err(RuntimeError::BadPointer);
                }
                let mut native = bytes.clone();
                native.reverse();
                native
            }
            (RuntimeRep::Void, Atom::Void) => continue,
            _ => return Err(RuntimeError::BadPointer),
        };
        unsafe {
            std::ptr::copy_nonoverlapping(value.as_ptr(), object.add(address), value.len());
        }
    }
    Ok(())
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

pub(super) fn runtime_error(machine: &MachineState, error: RuntimeError) -> ExecutionError {
    machine.set_first_cause(error);
    runtime_error_from_machine(machine)
}

pub(super) fn runtime_error_from_machine(machine: &MachineState) -> ExecutionError {
    ExecutionError::Runtime(machine.last_failure().unwrap_or(MachineFailure {
        cause: RuntimeError::BadPointer,
        disposition: MachineDisposition::Unavailable,
    }))
}

pub(super) fn runtime_error_for_status(
    machine: &MachineState,
    status: CallStatus,
) -> ExecutionError {
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
