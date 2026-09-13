use super::plan::HeapTopSpec;
use super::{CompiledProgram, ObservationFailure, Unsupported};
use crate::host_fns::RuntimeError;
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
        let mut invocation =
            super::invocation::PreparedInvocation::enter(self, entry, arguments, options, cancel)?;
        invocation.observe(options.observation_budget)
    }
}

pub(super) fn try_words(words: usize) -> Result<Vec<u64>, ExecutionError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(words)
        .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
    result.resize(words, 0);
    Ok(result)
}

pub(super) fn try_root_words(words: usize) -> Result<super::invocation::RootWords, ExecutionError> {
    super::invocation::RootWords::new(words)
}

pub(super) fn heap_top_extent(specs: &[HeapTopSpec]) -> Result<usize, ExecutionError> {
    specs.iter().try_fold(0usize, |total, spec| {
        total
            .checked_add(spec.descriptor.allocation_extent() as usize)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::HeapOverflow))
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "heap-top initialization independently borrows nursery bounds, binding specifications, root custody, static storage, and pinned-byte storage"
)]
pub(super) fn initialize_heap_tops(
    start: *mut u8,
    capacity: usize,
    specs: &[HeapTopSpec],
    top_slots: &std::collections::BTreeMap<ValueId, usize>,
    top_table: &super::invocation::RootWords,
    statics: &tidepool_heap::static_region::StaticRegion,
    byte_tops: &std::collections::BTreeMap<ValueId, Arc<[u8]>>,
    bytes: &super::static_bytes::PinnedBytes,
) -> Result<usize, RuntimeError> {
    let mut offsets = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for spec in specs {
        offsets.insert(spec.id, total);
        total = total
            .checked_add(spec.descriptor.allocation_extent() as usize)
            .ok_or(RuntimeError::HeapOverflow)?;
    }
    if total > capacity || !total.is_multiple_of(8) {
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
                    bytes,
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
                    bytes,
                )?;
            }
            HeapRhs::Bytes(_) => return Err(RuntimeError::BadPointer),
        }
        if let Some(&slot) = top_slots.get(&spec.id) {
            top_table
                .write(slot, pointer(spec.id)? as u64)
                .map_err(|_| RuntimeError::BadPointer)?;
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
    bytes: &super::static_bytes::PinnedBytes,
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
                tidepool_repr::execution_schema::ScalarLiteral::Bytes(literal) => bytes
                    .get(literal)
                    .map(|storage| storage.as_ptr() as usize)
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

pub(super) fn register_result_roots(
    machine: &MachineState,
    result_area: &super::invocation::RootWords,
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

pub(super) fn runtime_error_from_machine_or_observation(
    machine: &MachineState,
    observation: ObservationFailure,
) -> ExecutionError {
    if machine.last_failure().is_some() {
        runtime_error_from_machine(machine)
    } else {
        ExecutionError::Observation(observation)
    }
}

pub(super) fn runtime_error_without_machine(error: RuntimeError) -> ExecutionError {
    ExecutionError::Runtime(MachineFailure {
        disposition: error.machine_disposition(),
        cause: error,
    })
}
