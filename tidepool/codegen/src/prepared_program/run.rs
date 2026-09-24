use super::machine::ProgramId;
use super::plan::{HeapTopSpec, ImportSlot};
use super::{CompiledProgram, ObservationFailure, Unsupported};
use crate::host_fns::RuntimeError;
use crate::machine_state::MachineFailure;
use crate::machine_state::{MachineDisposition, MachineState};
use crate::prepared_control::CallStatus;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_bridge::HaskellValue;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::ValueId;
use tidepool_repr::execution_schema::{Atom, HeapRhs, RuntimeRep, SymbolIdentity, ValueRef};

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
    pub values: Vec<HaskellValue>,
    pub collections: u64,
}

/// One runtime shape fact compared while resolving a declared import against
/// the live [`super::PreparedHandle`] a caller actually supplied --
/// representation, or settledness. Identity, signature and generation were
/// already proven by [`tidepool_repr::execution_schema::link_program`]; this
/// is what codegen alone can only know once a live handle exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportShapeFact {
    Representation(RuntimeRep),
    Evaluated(bool),
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("entry {0:?} is not exported by this program")]
    MissingEntry(ValueId),
    #[error("entry arguments: expected {expected} physical scalar slots, got {actual}")]
    Arguments { expected: usize, actual: usize },
    #[error("entry argument {index} has representation {actual:?}; expected {expected:?}")]
    ArgumentRepresentation {
        index: usize,
        expected: RuntimeRep,
        actual: RuntimeRep,
    },
    #[error("prepared managed handle is unknown, foreign, or released")]
    UnknownPreparedHandle,
    #[error("import {identity:?} shape mismatch: expected {expected:?}, found {found:?}")]
    ImportShape {
        // Boxed: `SymbolIdentity` carries four owned `String`s, which would
        // otherwise make this the dominant variant in `ExecutionError`'s
        // size (clippy::result_large_err on every `Result<_, ExecutionError>`
        // return, workspace-wide).
        identity: Box<SymbolIdentity>,
        expected: ImportShapeFact,
        found: ImportShapeFact,
    },
    #[error("program {0:?} is not installed on this machine")]
    UnknownProgram(ProgramId),
    /// No frame is parked under this id on this machine: never parked here,
    /// already taken by a resume or abort, or dropped when its resource scope closes.
    #[error("continuation {0:?} is not parked on this machine")]
    UnknownContinuation(crate::suspension::ContinuationId),
    /// A managed value could not be built; no result handle was published.
    #[error("managed construction: {0}")]
    Answer(#[from] super::answer::AnswerBuildError),
    /// A major collection or retirement was requested while generated frames,
    /// temporary roots or an observation borrow were live, or before any
    /// program installed a heap. Nothing was moved or freed.
    #[error("the machine is not quiescent")]
    NotQuiescent,
    #[error("constructor host id {host_id:?} already names {existing:?}, not {identity:?}")]
    HostIdConflict {
        host_id: tidepool_repr::DataConId,
        identity: Box<tidepool_repr::execution_schema::SymbolIdentity>,
        existing: Box<tidepool_repr::execution_schema::SymbolIdentity>,
    },
    #[error(
        "constructor {identity:?} is declared differently from the descriptor this machine \
         already shares for it: interned field representations {existing_field_reps:?} \
         (arity {existing_arity}), incoming {incoming_field_reps:?} (arity {incoming_arity})",
        existing_arity = existing_field_reps.len(),
        incoming_arity = incoming_field_reps.len(),
    )]
    DescriptorShape {
        identity: Box<SymbolIdentity>,
        existing_field_reps: Vec<RuntimeRep>,
        incoming_field_reps: Vec<RuntimeRep>,
    },
    #[error("the program was compiled against another machine's external wrapper descriptors")]
    ForeignExternals,
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    #[error("{cause}", cause = .0.cause)]
    Runtime(MachineFailure),
    #[error(transparent)]
    Observation(#[from] ObservationFailure),
    #[error(transparent)]
    Static(#[from] tidepool_heap::static_region::StaticImageError),
    /// A condition this machine's own bookkeeping should never let happen,
    /// discovered at a caller/ordering boundary rather than while tracing
    /// heap content: it does not indicate the heap itself is untrustworthy,
    /// so it must not latch the machine (see [`runtime_error`]'s doc).
    #[error("prepared machine invariant: {0}")]
    Invariant(&'static str),
}

impl ExecutionError {
    /// Whether this failure is only an observation budget running out —
    /// materializing a value stopped because it ran past its byte/node
    /// ceiling, not because the program, the heap, or the value is actually
    /// wrong. The one classifier every caller that tolerates an exhausted
    /// budget (by cutting the walk instead of failing it, or by degrading a
    /// `Complete` observation to a bounded one) shares, so a caller does not
    /// re-derive this match against `Observation(BudgetExceeded)` itself.
    #[must_use]
    pub fn is_observation_budget_exhausted(&self) -> bool {
        matches!(self, Self::Observation(ObservationFailure::BudgetExceeded { .. }))
    }
}

impl CompiledProgram {
    pub fn run_entry(
        &self,
        entry: ValueId,
        arguments: &[u64],
        options: &RunOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunResult, ExecutionError> {
        let (mut machine, program) = super::machine::PreparedMachine::from_borrowed(
            self,
            super::machine::PreparedMachineOptions {
                nursery_bytes: options.nursery_bytes,
            },
        )?;
        machine.run_entry_with_raw_cancel(
            program,
            entry,
            arguments,
            super::machine::PreparedCallOptions {
                observation_budget: options.observation_budget,
                collect_before_observation: options.collect_before_observation,
            },
            cancel,
        )
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

pub(super) fn try_root_words(words: usize) -> Result<super::roots::RootWords, ExecutionError> {
    super::roots::RootWords::new(words)
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
    top_table: &super::roots::RootWords,
    statics: &tidepool_heap::static_region::StaticRegion,
    byte_tops: &std::collections::BTreeMap<ValueId, Arc<[u8]>>,
    bytes: &super::static_bytes::PinnedBytes,
    import_slots: &[ImportSlot],
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
            let descriptor = descriptors
                .get(&id)
                .ok_or_else(|| crate::host_fns::bad_pointer())?;
            return base
                .checked_add(*offset)
                .and_then(|address| address.checked_add(usize::from(descriptor.tag())))
                .ok_or_else(|| crate::host_fns::bad_pointer());
        }
        statics
            .entry(id)
            .ok_or_else(|| crate::host_fns::bad_pointer())
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
                    top_table,
                    import_slots,
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
                    top_table,
                    import_slots,
                )?;
            }
            HeapRhs::Bytes(_) => return Err(crate::host_fns::bad_pointer()),
        }
        if let Some(&slot) = top_slots.get(&spec.id) {
            top_table
                .write(slot, pointer(spec.id)? as u64)
                .map_err(|_| crate::host_fns::bad_pointer())?;
        }
    }
    Ok(total)
}

#[expect(
    clippy::too_many_arguments,
    reason = "static-atom writing independently borrows the object, descriptor, atom/rep pairs, pointer resolver, byte-top and pinned-bytes storage, and (for a Global reference) the machine-wide top table and this program's import slots"
)]
fn write_atoms(
    object: *mut u8,
    descriptor: &ObjectDescriptor,
    atoms: &[Atom],
    reps: &[RuntimeRep],
    pointer: &impl Fn(ValueId) -> Result<usize, RuntimeError>,
    byte_tops: &std::collections::BTreeMap<ValueId, Arc<[u8]>>,
    bytes: &super::static_bytes::PinnedBytes,
    top_table: &super::roots::RootWords,
    import_slots: &[ImportSlot],
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
                    return Err(crate::host_fns::bad_pointer());
                }
                value.to_ne_bytes().to_vec()
            }
            (RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef, Atom::Ref(ValueRef::Global(id))) => {
                // Read the import's CURRENT published slot value, not a
                // cached pointer: this heap top is initialized only after
                // `install` has already published every import slot (see
                // `machine.rs::install`'s ordering), so the slot must
                // already hold a live, non-zero pointer -- a zero here means
                // an import was read before it was published, which this
                // function must never silently trust regardless of the
                // caller's ordering discipline.
                let slot = import_slots
                    .get(id.0 as usize)
                    .ok_or_else(|| crate::host_fns::bad_pointer())?
                    .slot;
                let value = top_table.read(slot)?;
                if value == 0 {
                    return Err(crate::host_fns::bad_pointer());
                }
                if field.size() as usize != std::mem::size_of::<usize>() {
                    return Err(crate::host_fns::bad_pointer());
                }
                value.to_ne_bytes().to_vec()
            }
            (RuntimeRep::Address, Atom::Ref(ValueRef::Local(id))) => byte_tops
                .get(id)
                .map(|bytes| bytes.as_ptr() as usize)
                .ok_or_else(|| crate::host_fns::bad_pointer())?
                .to_ne_bytes()
                .to_vec(),
            (RuntimeRep::Address, Atom::Scalar(literal)) => match literal {
                tidepool_repr::execution_schema::ScalarLiteral::NullAddress => {
                    vec![0; field.size() as usize]
                }
                tidepool_repr::execution_schema::ScalarLiteral::Bytes(literal) => bytes
                    .get(literal)
                    .map(|storage| storage.as_ptr() as usize)
                    .ok_or_else(|| crate::host_fns::bad_pointer())?
                    .to_ne_bytes()
                    .to_vec(),
                _ => return Err(crate::host_fns::bad_pointer()),
            },
            (
                RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_),
                Atom::Scalar(literal),
            ) => {
                let bytes = match literal {
                    tidepool_repr::execution_schema::ScalarLiteral::Int { bytes, .. }
                    | tidepool_repr::execution_schema::ScalarLiteral::Word { bytes, .. }
                    | tidepool_repr::execution_schema::ScalarLiteral::Float { bytes, .. } => bytes,
                    _ => return Err(crate::host_fns::bad_pointer()),
                };
                if bytes.len() != field.size() as usize {
                    return Err(crate::host_fns::bad_pointer());
                }
                let mut native = bytes.clone();
                native.reverse();
                native
            }
            (RuntimeRep::Void, Atom::Void) => continue,
            _ => return Err(crate::host_fns::bad_pointer()),
        };
        unsafe {
            std::ptr::copy_nonoverlapping(value.as_ptr(), object.add(address), value.len());
        }
    }
    Ok(())
}

pub(super) fn register_result_roots(
    machine: &MachineState,
    result_area: &super::roots::RootWords,
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

/// Report a host-side failure. An integrity cause latches the machine; a
/// reusable cause is this operation's own outcome and is reported directly
/// (a cause the running call already recorded takes precedence, as it does
/// for generated code). Recording a reusable cause here would leave a stale
/// pending outcome behind an observation or install, which runs outside any
/// call and so has no `begin`/`end` bracket to settle it.
pub(super) fn runtime_error(machine: &MachineState, error: RuntimeError) -> ExecutionError {
    if error.machine_disposition() == MachineDisposition::Unavailable {
        machine.set_first_cause(error);
        return runtime_error_from_machine(machine);
    }
    match machine.current_failure() {
        Some(failure) => ExecutionError::Runtime(failure),
        None => runtime_error_without_machine(error),
    }
}

/// The failure a completing call reports: the machine latch when set,
/// otherwise this call's own pending outcome (see
/// `MachineState::current_failure`). A machine with no recorded cause at all
/// (never latched, never reported by the completing call) is itself an
/// invariant violation distinct from any diagnosed failure.
pub(super) fn runtime_error_from_machine(machine: &MachineState) -> ExecutionError {
    ExecutionError::Runtime(machine.current_failure().unwrap_or(MachineFailure {
        cause: RuntimeError::StatusWithoutCause(CallStatus::IntegrityFailure),
        disposition: MachineDisposition::Unavailable,
    }))
}

/// A failure status arrived from a completing call with no cause recorded:
/// give it its own diagnosis (`StatusWithoutCause`) rather than folding it
/// into `BadPointer`, so a missing-cause defect is never confused with an
/// actual bad-pointer check failing.
pub(super) fn runtime_error_for_status(
    machine: &MachineState,
    status: CallStatus,
) -> ExecutionError {
    if machine.current_failure().is_none() {
        machine.set_first_cause(match status {
            CallStatus::Cancelled => RuntimeError::Cancelled,
            CallStatus::LanguageFailure | CallStatus::IntegrityFailure | CallStatus::Success => {
                RuntimeError::StatusWithoutCause(status)
            }
        });
    }
    runtime_error_from_machine(machine)
}

/// Result observation right after a call: a failure the call itself
/// recorded takes precedence over the observation's own complaint.
pub(super) fn runtime_error_from_machine_or_observation(
    machine: &MachineState,
    observation: ObservationFailure,
) -> ExecutionError {
    if machine.current_failure().is_some() {
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
