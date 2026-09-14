//! Persistent ownership for one prepared compiled program.
//!
//! The machine owns every address that generated code can retain.  A call only
//! borrows it long enough to install its cancellation attachment and temporary
//! result roots; neither attachment survives the native return.

use super::roots::{OldSpaceScope, RootWords};
use super::run::{
    heap_top_extent, initialize_heap_tops, register_result_roots, runtime_error,
    runtime_error_for_status, runtime_error_from_machine,
    runtime_error_from_machine_or_observation, runtime_error_without_machine, try_root_words,
    try_words,
};
use super::safepoint::NativeStackBounds;
use super::{CompiledProgram, ExecutionError, RunResult, Unsupported};
use crate::context::VMContext;
use crate::host_fns::{gc_trigger, prepared_gc_trigger, RuntimeError};
use crate::machine_state::{MachineDisposition, MachineFailure, MachineState};
use crate::old_space::OldSpace;
use crate::prepared_control::CallStatus;
use crate::resource_ledger::RootHandleLedger;
use crate::suspension::{RealmId, ValueHandle};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, ValueId};
use tidepool_repr::DataConId;

/// A compiled program and its one persistent mutable prepared-STG substrate.
///
/// This is deliberately !Send: code custody, its VM context, and every live
/// heap root stay on the thread that enters generated code.  A later resident
/// owner may stow the whole machine under its existing single-owner protocol;
/// it must not split these fields into independent registries.
enum ProgramCustody<'code> {
    Borrowed(&'code CompiledProgram),
    Owned(Rc<CompiledProgram>),
}

impl ProgramCustody<'_> {
    fn get(&self) -> &CompiledProgram {
        match self {
            Self::Borrowed(program) => program,
            Self::Owned(program) => program,
        }
    }
}

pub struct PreparedMachine<'code> {
    program: ProgramCustody<'code>,
    machine: Rc<MachineState>,
    vmctx: VMContext,
    statics: Arc<StaticRegion>,
    _top_table: RootWords,
    old_space: Box<OldSpace>,
    handles: RootHandleLedger,
}

/// Immutable capacity selected when a prepared machine is installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedMachineOptions {
    pub nursery_bytes: usize,
}

/// Per-entry behavior that does not alter the resident machine's capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedCallOptions {
    pub observation_budget: usize,
    pub collect_before_observation: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedHandle {
    raw: ValueHandle,
    rep: RuntimeRep,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedInput {
    Scalar(u64),
    Managed(PreparedHandle),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedResult {
    Void,
    Scalar(u64),
    Managed(PreparedHandle),
}

#[derive(Debug)]
pub struct PreparedResultBatch {
    pub values: Vec<PreparedResult>,
    pub collections: u64,
}

/// One constructor layer read without evaluating any field.
///
/// Managed fields are retained as fresh handles.  Callable fields remain
/// opaque: inspection never enters them or invokes generated code.
#[derive(Debug)]
pub enum PreparedOuter {
    Constructor {
        identity: DataConId,
        fields: Vec<PreparedResult>,
    },
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

impl PreparedMachine<'static> {
    /// Install `program` once and retain its mutable heap, static image, top
    /// table, descriptor registry and compiled code until this owner drops.
    pub fn new(
        program: CompiledProgram,
        options: PreparedMachineOptions,
    ) -> Result<Self, ExecutionError> {
        Self::install(ProgramCustody::Owned(Rc::new(program)), options)
    }
}

impl<'code> PreparedMachine<'code> {
    /// Temporary compatibility owner for the direct compiled-program API.
    /// Runtime persistence always uses [`Self::new`], whose code custody is
    /// owned rather than borrowed.
    pub(crate) fn from_borrowed(
        program: &'code CompiledProgram,
        options: PreparedMachineOptions,
    ) -> Result<Self, ExecutionError> {
        Self::install(ProgramCustody::Borrowed(program), options)
    }

    fn install(
        program: ProgramCustody<'code>,
        options: PreparedMachineOptions,
    ) -> Result<Self, ExecutionError> {
        let compiled = program.get();
        let statics = Arc::new(compiled.statics.instantiate()?);
        let top_table = try_root_words(compiled.top_slots.len())?;
        for (&id, &slot) in &compiled.top_slots {
            if compiled.heap_top_specs.iter().any(|spec| spec.id == id) {
                continue;
            }
            let value = statics
                .entry(id)
                .or_else(|| {
                    compiled
                        .byte_tops
                        .get(&id)
                        .map(|bytes| bytes.as_ptr() as usize)
                })
                .ok_or(ExecutionError::MissingEntry(id))?;
            top_table.write(slot, value as u64)?;
        }

        let heap_reserve = heap_top_extent(&compiled.heap_top_specs)?;
        let nursery = try_words(
            options
                .nursery_bytes
                .max(heap_reserve)
                .div_ceil(std::mem::size_of::<u64>()),
        )?;
        let machine = Rc::new(MachineState::new());
        machine.set_stack_map_registry(&compiled.pipeline.stack_maps);
        if let Err(error) = machine.install_prepared_buffer_with_static_region(
            nursery,
            compiled.descriptors.clone(),
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
            &compiled.heap_top_specs,
            &compiled.top_slots,
            &top_table,
            &statics,
            &compiled.byte_tops,
            &compiled.bytes,
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
        for spec in &compiled.heap_top_specs {
            if let Some(&slot) = compiled.top_slots.get(&spec.id) {
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
            handles: RootHandleLedger::default(),
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

    /// Release one retained managed result. Unknown or foreign values do not
    /// expose a slot and therefore cannot affect a later entry.
    pub fn release(&mut self, handle: PreparedHandle) -> bool {
        let Some(entry) = self.handles.take(handle.raw) else {
            return false;
        };
        self.machine.deregister_persistent_root(entry.slot.addr());
        true
    }

    /// Inspect one constructor layer of a retained value without forcing it.
    ///
    /// Every managed field receives its own persistent root before the
    /// descriptor reader releases the active nursery borrow.  The source
    /// handle remains owned by this machine and can be inspected again.
    pub fn inspect_outer(
        &mut self,
        handle: PreparedHandle,
    ) -> Result<PreparedOuter, ExecutionError> {
        self.ensure_handle_access()?;
        let source = self
            .handles
            .get(handle.raw)
            .filter(|entry| entry.realm == RealmId::ROOT)
            .map(|entry| entry.slot)
            .ok_or(ExecutionError::UnknownPreparedHandle)?;
        let word = unsafe { source.current() } as usize;
        if word == 0 {
            return Err(ExecutionError::UnknownPreparedHandle);
        }
        let (identity, fields) = self.inspect_constructor(super::observe::ObservationSeed {
            word,
            rep: handle.rep,
        })?;
        let words = RootWords::new(fields.len())?;
        let mut managed = Vec::new();
        let mut output = Vec::new();
        managed
            .try_reserve_exact(fields.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        output
            .try_reserve_exact(fields.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        for (index, field) in fields.iter().copied().enumerate() {
            words.write(index, field.word as u64)?;
            match field.rep {
                RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef => {
                    managed.push((index, field.rep));
                    output.push(PreparedResult::Void);
                }
                RuntimeRep::Void => {
                    return Err(ExecutionError::Observation(
                        super::ObservationFailure::Integrity(
                            tidepool_heap::execution_descriptor::DescriptorTraceError::InvalidRange,
                        ),
                    ))
                }
                RuntimeRep::Address => {
                    return Err(
                        super::ObservationFailure::Representation(RuntimeRep::Address).into(),
                    )
                }
                RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_) => {
                    output.push(PreparedResult::Scalar(field.word as u64));
                }
            }
        }
        self.handles
            .try_reserve(managed.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(managed.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        for &(index, _) in &managed {
            selected.push(unsafe { words.as_mut_ptr().add(index).cast::<*mut u8>() });
        }
        let mark = self.machine.rust_roots_len();
        for &(index, _) in &managed {
            let slot = unsafe { words.as_mut_ptr().add(index).cast::<*mut u8>() };
            self.machine.register_rust_root(slot);
        }
        let _roots = TemporaryRoots {
            machine: &self.machine,
            mark,
        };
        if !managed.is_empty() {
            if unsafe { self.machine.prepared_old_space() }.is_some() {
                return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
            }
            unsafe { self.machine.install_prepared_old_space(&self.old_space) };
            let retained = unsafe {
                self.old_space.retain_prepared(
                    &self.machine,
                    &mut self.vmctx,
                    &selected,
                    &self.program.get().descriptors,
                )
            };
            self.machine.clear_prepared_old_space();
            let roots = retained.map_err(|cause| runtime_error(&self.machine, cause))?;
            if roots.len() != managed.len() {
                for root in roots {
                    self.machine.deregister_persistent_root(root.addr());
                }
                return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
            }
            for ((index, rep), root) in managed.into_iter().zip(roots) {
                let raw = self.handles.insert(root, RealmId::ROOT);
                output[index] = PreparedResult::Managed(PreparedHandle { raw, rep });
            }
        }
        Ok(PreparedOuter::Constructor {
            identity,
            fields: output,
        })
    }

    fn ensure_handle_access(&self) -> Result<(), ExecutionError> {
        if self.machine.disposition() == MachineDisposition::Unavailable {
            return Err(ExecutionError::Runtime(
                self.machine.last_failure().unwrap_or(MachineFailure {
                    cause: RuntimeError::BadPointer,
                    disposition: MachineDisposition::Unavailable,
                }),
            ));
        }
        Ok(())
    }

    /// Execute with representation-checked values and retain every managed
    /// result before its temporary adapter storage can disappear.
    pub fn run_entry_retained(
        &mut self,
        entry: ValueId,
        arguments: &[PreparedInput],
        options: PreparedCallOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<PreparedResultBatch, ExecutionError> {
        let (adapter, reps, result_contract, result_layout) = {
            let compiled = self
                .program
                .get()
                .entries
                .get(&entry)
                .ok_or(ExecutionError::MissingEntry(entry))?;
            (
                compiled.adapter,
                compiled.abi.physical_arguments().to_vec(),
                compiled.abi.semantic_results().clone(),
                compiled.abi.result_layout().clone(),
            )
        };
        if arguments.len() != reps.len() {
            return Err(ExecutionError::Arguments {
                expected: reps.len(),
                actual: arguments.len(),
            });
        }
        let argument_area = RootWords::new(arguments.len())?;
        let mut managed_arguments = Vec::new();
        for (index, (argument, expected)) in arguments.iter().zip(&reps).enumerate() {
            let word = match (argument, expected) {
                (PreparedInput::Scalar(word), actual)
                    if !matches!(actual, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) =>
                {
                    *word
                }
                (PreparedInput::Managed(handle), actual)
                    if *actual == handle.rep
                        && matches!(actual, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) =>
                {
                    let entry = self
                        .handles
                        .get(handle.raw)
                        .ok_or(ExecutionError::UnknownPreparedHandle)?;
                    let word = unsafe { entry.slot.current() } as usize as u64;
                    if word == 0 {
                        return Err(ExecutionError::UnknownPreparedHandle);
                    }
                    managed_arguments.push(index);
                    word
                }
                (PreparedInput::Managed(handle), actual) => {
                    return Err(ExecutionError::ArgumentRepresentation {
                        index,
                        expected: *actual,
                        actual: handle.rep,
                    });
                }
                (PreparedInput::Scalar(_), actual) => {
                    return Err(ExecutionError::ArgumentRepresentation {
                        index,
                        expected: *actual,
                        actual: RuntimeRep::Word(64),
                    });
                }
            };
            argument_area.write(index, word)?;
        }
        let argument_mark = self.machine.rust_roots_len();
        for index in managed_arguments {
            let slot = unsafe { argument_area.as_mut_ptr().add(index).cast::<*mut u8>() };
            self.machine.register_rust_root(slot);
        }
        let _arguments = TemporaryRoots {
            machine: &self.machine,
            mark: argument_mark,
        };
        let max_native_frame = self.program.get().pipeline.native_frame_maximum();
        let reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(reserve)
            .map_err(runtime_error_without_machine)?;
        self.vmctx.prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;
        self.machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        self.machine.set_cancel_flag(cancel);
        let _cancel = CancelScope(&self.machine);
        let result_words =
            (result_layout.payload_size() as usize).div_ceil(std::mem::size_of::<u64>());
        let results = try_root_words(result_words.max(1))?;
        let collections_before = self.machine.gc_generation();
        let pointer = self.program.get().pipeline.get_function_ptr(adapter);
        let raw = {
            let _scope = OldSpaceScope::new(&self.machine, &self.old_space)?;
            unsafe {
                let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                    std::mem::transmute(pointer);
                adapter(
                    &mut self.vmctx,
                    results.as_mut_ptr(),
                    argument_area.as_mut_ptr(),
                )
            }
        };
        let status = CallStatus::from_raw(i64::from(raw))
            .map_err(|_| runtime_error(&self.machine, RuntimeError::BadPointer))?;
        if status != CallStatus::Success
            || self.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(runtime_error_for_status(&self.machine, status));
        }
        let result_reps = result_contract
            .returned_reps()
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::NoSuccessReturned))?;
        let mark = self.machine.rust_roots_len();
        register_result_roots(&self.machine, &results, &result_layout);
        let _results = TemporaryRoots {
            machine: &self.machine,
            mark,
        };
        if options.collect_before_observation {
            Self::collect_on(&self.machine, &mut self.vmctx, &self.old_space, 0)?;
        }
        let mut slots = Vec::new();
        let mut output = Vec::new();
        for (logical, rep) in result_reps.iter().copied().enumerate() {
            let Some(stored) = result_layout
                .logical_to_stored()
                .get(logical)
                .copied()
                .flatten()
            else {
                output.push(PreparedResult::Void);
                continue;
            };
            let field = &result_layout.fields()[stored as usize];
            let address = unsafe {
                results
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(field.offset() as usize)
            };
            if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
                slots.push(address.cast::<*mut u8>());
                output.push(PreparedResult::Void);
            } else {
                let mut bytes = [0_u8; 8];
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        address,
                        bytes.as_mut_ptr(),
                        field.size() as usize,
                    )
                };
                output.push(PreparedResult::Scalar(u64::from_ne_bytes(bytes)));
            }
        }
        if !slots.is_empty() {
            if unsafe { self.machine.prepared_old_space() }.is_some() {
                return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
            }
            // Promotion mutates OldSpace, so its admission pointer is scoped
            // manually rather than held through an immutable Rust borrow.
            unsafe { self.machine.install_prepared_old_space(&self.old_space) };
            let retained = unsafe {
                self.old_space.retain_prepared(
                    &self.machine,
                    &mut self.vmctx,
                    &slots,
                    &self.program.get().descriptors,
                )
            };
            self.machine.clear_prepared_old_space();
            let roots = retained.map_err(|cause| runtime_error(&self.machine, cause))?;
            if roots.len() != slots.len() {
                for root in roots {
                    self.machine.deregister_persistent_root(root.addr());
                }
                return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
            }
            let managed =
                result_reps.iter().copied().enumerate().filter(|(_, rep)| {
                    matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                });
            for ((logical, rep), root) in managed.zip(roots) {
                let raw = self.handles.insert(root, RealmId::ROOT);
                output[logical] = PreparedResult::Managed(PreparedHandle { raw, rep });
            }
        }
        Ok(PreparedResultBatch {
            values: output,
            collections: self
                .machine
                .gc_generation()
                .saturating_sub(collections_before),
        })
    }

    fn inspect_constructor(
        &self,
        seed: super::observe::ObservationSeed,
    ) -> Result<(DataConId, Vec<super::observe::ObservationSeed>), ExecutionError> {
        let (start, size) = self
            .machine
            .gc_active_range()
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::BadPointer))?;
        let cursor = (self.vmctx.alloc_ptr as usize)
            .checked_sub(start as usize)
            .filter(|cursor| *cursor <= size && *cursor % std::mem::size_of::<u64>() == 0)
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::BadPointer))?;
        let nursery = unsafe {
            std::slice::from_raw_parts(start.cast::<u64>(), cursor / std::mem::size_of::<u64>())
        };
        let mut starts = Vec::new();
        let mut scanned_words = 0;
        super::observe::append_exact_starts(
            nursery,
            &self.program.get().descriptor_registry,
            &mut starts,
            &mut scanned_words,
        )?;
        let heap = super::observe::ObservationHeap::new_with_registry_and_starts(
            nursery,
            &self.statics,
            &self.program.get().descriptor_registry,
            &starts,
            Some(&*self.old_space),
            &self.machine,
        )?;
        heap.inspect_constructor(seed).map_err(ExecutionError::from)
    }

    #[cfg(test)]
    pub(crate) fn persistent_roots_count(&self) -> usize {
        self.machine.persistent_roots_count()
    }

    #[cfg(test)]
    pub(crate) fn top_words(&self) -> Vec<u64> {
        self._top_table.snapshot()
    }

    /// Execute one scalar-only entry on the retained machine.
    pub fn run_entry(
        &mut self,
        entry: ValueId,
        arguments: &[u64],
        options: PreparedCallOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunResult, ExecutionError> {
        let (adapter, expected_arguments, has_managed_arguments, result_contract, result_layout) = {
            let compiled = self
                .program
                .get()
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
        let max_native_frame = self.program.get().pipeline.native_frame_maximum();
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
        let pointer = self.program.get().pipeline.get_function_ptr(adapter);
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
            self.program.get(),
            &mut self.vmctx,
            &self.statics,
            &self.program.get().descriptor_registry,
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

impl Drop for PreparedMachine<'_> {
    fn drop(&mut self) {
        self.machine.clear_prepared_old_space();
        self.machine.clear_rust_roots();
        for (start, end) in self.machine.old_space_arena_ranges() {
            self.machine.retire_old_space_arena(start, end);
        }
        self.machine.free_session_heap();
        self.machine.clear_stack_map_registry();
        self.machine.clear_cancel_flag();
        self.vmctx.machine_state = std::ptr::null_mut();
        self.vmctx.prepared_tops = std::ptr::null();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_fns::RuntimeError;
    use crate::machine_state::MachineDisposition;
    use crate::prepared_program::entry_tests::caf_program;
    use crate::prepared_program::{
        ExecutionError, PreparedCallOptions, PreparedMachineOptions, RunOptions,
    };
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{
        link_program, parse_program, testing, Architecture, Atom, CheckedLayout, ConstructorDecl,
        ConstructorId, DecodeLimits, Endianness, ExprFrame, FieldLayout, Group, HeapBinding,
        HeapRhs, MachineImports, ProgramRequirements, ResultContract, RuntimeRep, ScalarLiteral,
        Signature, SignatureId, TargetDescriptor, TopBinding, UpdatePolicy, ValueId, ValueRef,
        EXECUTION_ABI_VERSION, SCHEMA_VERSION,
    };

    fn machine() -> PreparedMachine<'static> {
        PreparedMachine::new(
            caf_program(0, false, UpdatePolicy::Memoize),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine")
    }

    fn language_failure_program() -> CompiledProgram {
        let mut wire = super::super::no_success_tests::raised_caf();
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 7_i64.to_be_bytes().to_vec(),
            })]));
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("PreparedMachine", "success"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Function {
                    signature: SignatureId(2),
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 7_i64.to_be_bytes().to_vec(),
            })]));
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("PreparedMachine", "managed"),
            binding: HeapBinding {
                id: ValueId(3),
                rhs: HeapRhs::Function {
                    signature: SignatureId(3),
                    parameters: vec![ValueId(77)],
                    captures: vec![],
                    body: 2,
                },
            },
        }));
        let prepared = testing::prepare(wire).expect("language failure fixture");
        let linked =
            tidepool_repr::execution_schema::link_program(prepared, &MachineImports::default())
                .expect("language failure fixture links");
        CompiledProgram::compile(&linked).expect("language failure fixture compiles")
    }

    fn managed_roundtrip_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("PreparedMachine", "Unit"),
            family: testing::identity("PreparedMachine", "Unit"),
            host_id: tidepool_repr::DataConId(990),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
                ValueId(99),
            ))]));
        let mut body = 1;
        for id in 0..32 {
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(100 + id),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                }),
                body,
            });
            body = wire.expressions.nodes.len() - 1;
        }
        wire.bindings = vec![
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedMachine", "producer"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedMachine", "consumer"),
                binding: HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![ValueId(99)],
                        captures: vec![],
                        body,
                    },
                },
            }),
        ];
        let prepared = testing::prepare(wire).expect("roundtrip fixture");
        let linked =
            tidepool_repr::execution_schema::link_program(prepared, &MachineImports::default())
                .expect("roundtrip links");
        CompiledProgram::compile(&linked).expect("roundtrip compiles")
    }

    fn outer_with_function_field_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("PreparedOuter", "Envelope"),
            family: testing::identity("PreparedOuter", "Envelope"),
            host_id: tidepool_repr::DataConId(991),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            strict_fields: vec![false, false],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 8,
                    },
                ],
                alignment: 8,
                payload_size: 16,
                root_mask: vec![true, true],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("PreparedOuter", "Unit"),
            family: testing::identity("PreparedOuter", "Unit"),
            host_id: tidepool_repr::DataConId(992),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes = vec![
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![
                    Atom::Ref(ValueRef::Local(ValueId(1))),
                    Atom::Ref(ValueRef::Local(ValueId(2))),
                ],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
        ];
        wire.bindings = vec![
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedOuter", "producer"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedOuter", "continuation"),
                binding: HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![],
                        captures: vec![],
                        body: 1,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedOuter", "unforced"),
                binding: HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::SingleEntry,
                        captures: vec![],
                        body: 2,
                    },
                },
            }),
        ];
        let prepared = testing::prepare(wire).expect("prepared outer fixture");
        let linked =
            tidepool_repr::execution_schema::link_program(prepared, &MachineImports::default())
                .expect("prepared outer fixture links");
        CompiledProgram::compile(&linked).expect("prepared outer fixture compiles")
    }

    fn freer_retention_program() -> (CompiledProgram, ValueId, DataConId) {
        let requirements = ProgramRequirements {
            schema_version: SCHEMA_VERSION,
            projection_profile: "ghc-9.12-prepared-stg".into(),
            toolchain: "ghc-9.12.2".into(),
            execution_abi_version: EXECUTION_ABI_VERSION,
            target: TargetDescriptor {
                architecture: Architecture::X86_64,
                endianness: Endianness::Little,
                pointer_width: 64,
                word_width: 64,
                abi: "sysv64".into(),
                features: vec![],
            },
        };
        let prepared = parse_program(
            include_bytes!("../../../haskell/test-prepared-stg/fixtures/freer-retention.cbor"),
            &requirements,
            DecodeLimits::default(),
        )
        .expect("FreerRetention artifact parses");
        let entry = prepared.entry();
        let effect = prepared
            .constructors()
            .iter()
            .find(|constructor| constructor.identity.occurrence == "E")
            .expect("FreerRetention artifact includes the real freer E constructor")
            .host_id;
        let linked = link_program(prepared, &MachineImports::default())
            .expect("FreerRetention artifact links");
        (
            CompiledProgram::compile(&linked).expect("FreerRetention artifact compiles"),
            entry,
            effect,
        )
    }

    #[test]
    fn cancellation_is_recoverable_before_a_following_entry() {
        let mut machine = machine();
        let cancelled = Arc::new(AtomicBool::new(true));

        let error = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                Arc::clone(&cancelled),
            )
            .expect_err("cancelled entry must not publish a result");
        assert!(matches!(
            error,
            ExecutionError::Runtime(failure)
                if failure.cause == RuntimeError::Cancelled
                    && failure.disposition == MachineDisposition::Reusable
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("a settled cancellation must leave the machine reusable");
        assert_eq!(result.values.len(), 1);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn observation_failure_is_recoverable_before_a_following_entry() {
        let mut machine = machine();
        let error = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect_err("bounded observation must reject a constructor at zero budget");
        assert!(matches!(
            error,
            ExecutionError::Observation(super::super::ObservationFailure::BudgetExceeded {
                limit: 0
            })
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("an observation failure must not poison the prepared machine");
        assert_eq!(result.values.len(), 1);
    }

    #[test]
    fn language_failure_is_recoverable_before_a_following_entry() {
        let mut machine = PreparedMachine::new(
            language_failure_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let error = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect_err("raised entry must report a language failure");
        assert!(matches!(
            error,
            ExecutionError::Runtime(failure)
                if failure.cause == RuntimeError::RaisedException
                    && failure.disposition == MachineDisposition::Reusable
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(
                ValueId(2),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("a language failure must not poison the prepared machine");
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value))]
                if *value == 7
        ));
    }

    #[test]
    fn persistent_roots_survive_collection_between_successive_entries() {
        let mut machine = PreparedMachine::new(
            caf_program(
                0,
                false,
                tidepool_repr::execution_schema::UpdatePolicy::Memoize,
            ),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let initial_tops = machine.top_words();
        let persistent_roots = machine.persistent_roots_count();
        assert_eq!(persistent_roots, initial_tops.len());
        assert!(initial_tops.iter().all(|word| *word != 0));
        let first = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("first entry");
        let second = machine
            .run_entry(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("entry after collection");

        assert_eq!(second.collections, 1);
        assert!(matches!(
            first.values.as_slice(),
            [tidepool_bridge::Value::Con(id, fields)]
                if *id == tidepool_repr::DataConId(900) && fields.is_empty()
        ));
        assert!(matches!(
            second.values.as_slice(),
            [tidepool_bridge::Value::Con(id, fields)]
                if *id == tidepool_repr::DataConId(900) && fields.is_empty()
        ));
        assert_eq!(machine.persistent_roots_count(), persistent_roots);
        assert!(machine.top_words().iter().all(|word| *word != 0));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn machine_drop_clears_registered_roots_before_storage_drops() {
        let machine = machine();
        let state = Rc::clone(&machine.machine);
        assert!(state.persistent_roots_count() > 0);
        drop(machine);
        assert_eq!(state.persistent_roots_count(), 0);
        assert_eq!(state.rust_roots_len(), 0);
        assert!(state.old_space_arena_ranges().is_empty());
    }

    #[test]
    fn retained_managed_result_survives_collection_and_releases() {
        let mut machine = machine();
        let batch = machine
            .run_entry_retained(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("managed result is retained before frame cleanup");
        assert!(batch.collections >= 1);
        let [PreparedResult::Managed(handle)] = batch.values.as_slice() else {
            panic!("CAF must return one retained managed value");
        };
        assert!(machine.release(*handle));
        assert!(!machine.release(*handle));
    }

    #[test]
    fn managed_inputs_reject_foreign_and_rep_mismatches_before_entry() {
        let mut source = machine();
        let batch = source
            .run_entry_retained(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("source handle");
        let [PreparedResult::Managed(handle)] = batch.values.as_slice() else {
            panic!("source result must be managed");
        };
        let mut target = PreparedMachine::new(
            language_failure_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("target machine");
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        assert!(matches!(
            target.run_entry_retained(
                ValueId(3),
                &[PreparedInput::Managed(*handle)],
                options,
                Arc::new(AtomicBool::new(false)),
            ),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        let wrong_rep = PreparedHandle {
            raw: handle.raw,
            rep: RuntimeRep::UnliftedRef,
        };
        assert!(matches!(
            target.run_entry_retained(
                ValueId(3),
                &[PreparedInput::Managed(wrong_rep)],
                options,
                Arc::new(AtomicBool::new(false)),
            ),
            Err(ExecutionError::ArgumentRepresentation { .. })
        ));
    }

    #[test]
    fn managed_input_stays_rooted_through_collection_in_the_callee() {
        let mut machine = PreparedMachine::new(
            managed_roundtrip_program(),
            PreparedMachineOptions { nursery_bytes: 64 },
        )
        .expect("roundtrip machine");
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let producer = machine
            .run_entry_retained(ValueId(0), &[], options, Arc::new(AtomicBool::new(false)))
            .expect("producer result");
        let [PreparedResult::Managed(handle)] = producer.values.as_slice() else {
            panic!("producer must retain its constructor");
        };
        let consumer = machine
            .run_entry_retained(
                ValueId(1),
                &[PreparedInput::Managed(*handle)],
                options,
                Arc::new(AtomicBool::new(false)),
            )
            .expect("managed argument survives generated allocation");
        assert!(consumer.collections >= 1);
        let [PreparedResult::Managed(returned)] = consumer.values.as_slice() else {
            panic!("consumer must retain the returned managed argument");
        };
        assert!(machine.release(*handle));
        assert!(machine.release(*returned));
    }

    #[test]
    fn outer_inspection_retains_callable_fields_without_forcing_them() {
        let options = PreparedMachineOptions { nursery_bytes: 128 };
        let mut machine = PreparedMachine::new(outer_with_function_field_program(), options)
            .expect("prepared outer machine");
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: true,
        };
        let produced = machine
            .run_entry_retained(ValueId(0), &[], call, Arc::new(AtomicBool::new(false)))
            .expect("retained outer result");
        assert!(produced.collections >= 1);
        let [PreparedResult::Managed(outer)] = produced.values.as_slice() else {
            panic!("producer must return one managed outer result");
        };
        let PreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(*outer)
            .expect("outer inspection must not force its callable field");
        assert_eq!(identity, tidepool_repr::DataConId(991));
        let [PreparedResult::Managed(continuation), PreparedResult::Managed(unforced)] =
            fields.as_slice()
        else {
            panic!("outer inspection must retain callable and thunk fields");
        };
        assert!(matches!(
            machine.inspect_outer(*continuation),
            Err(ExecutionError::Observation(
                super::super::ObservationFailure::Unobservable(
                    tidepool_heap::execution_descriptor::ObjectKind::Function
                )
            ))
        ));
        assert!(matches!(
            machine.inspect_outer(*unforced),
            Err(ExecutionError::Observation(
                super::super::ObservationFailure::Unobservable(
                    tidepool_heap::execution_descriptor::ObjectKind::Thunk
                )
            ))
        ));

        let PreparedOuter::Constructor { fields, .. } = machine
            .inspect_outer(*outer)
            .expect("source handle remains live for repeated inspection");
        let [PreparedResult::Managed(second_continuation), PreparedResult::Managed(second_unforced)] =
            fields.as_slice()
        else {
            panic!("repeated inspection must retain fresh child handles");
        };

        let mut foreign = PreparedMachine::new(outer_with_function_field_program(), options)
            .expect("foreign prepared machine");
        assert!(matches!(
            foreign.inspect_outer(*outer),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        assert!(machine.release(*outer));
        assert!(matches!(
            machine.inspect_outer(*outer),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        assert!(machine.release(*continuation));
        assert!(machine.release(*unforced));
        assert!(machine.release(*second_continuation));
        assert!(machine.release(*second_unforced));
    }

    #[test]
    fn outer_inspection_refuses_an_unavailable_machine_before_handle_lookup() {
        let mut machine = machine();
        let batch = machine
            .run_entry_retained(
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("retained source handle");
        let [PreparedResult::Managed(handle)] = batch.values.as_slice() else {
            panic!("CAF must return one managed value");
        };
        machine.machine.set_first_cause(RuntimeError::BadPointer);
        assert!(matches!(
            machine.inspect_outer(*handle),
            Err(ExecutionError::Runtime(failure))
                if failure.cause == RuntimeError::BadPointer
                    && failure.disposition == MachineDisposition::Unavailable
        ));
    }

    #[test]
    fn real_freer_request_retains_its_continuation_across_another_collection() {
        let (program, entry, effect) = freer_retention_program();
        let mut machine = PreparedMachine::new(
            program,
            PreparedMachineOptions {
                nursery_bytes: 4096,
            },
        )
        .expect("FreerRetention machine");
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let first = machine
            .run_entry_retained(entry, &[], call, Arc::new(AtomicBool::new(false)))
            .expect("real freer request returns a retained outer value");
        let [PreparedResult::Managed(outer)] = first.values.as_slice() else {
            panic!("FreerRetention entry must return one managed E request");
        };
        let second = machine
            .run_entry_retained(
                entry,
                &[],
                PreparedCallOptions {
                    collect_before_observation: true,
                    ..call
                },
                Arc::new(AtomicBool::new(false)),
            )
            .expect("second real freer request collects without losing the first");
        assert!(second.collections >= 1);
        let [PreparedResult::Managed(second_outer)] = second.values.as_slice() else {
            panic!("second FreerRetention request must also be managed");
        };
        let PreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(*outer)
            .expect("first E request remains rooted after the later collection");
        assert_eq!(
            identity, effect,
            "descriptor metadata, not tag, identifies E"
        );
        let continuation = match fields.last() {
            Some(PreparedResult::Managed(handle)) => *handle,
            _ => panic!("the real E continuation field must remain an opaque managed handle"),
        };
        let children: Vec<_> = fields
            .iter()
            .filter_map(|field| match field {
                PreparedResult::Managed(handle) => Some(*handle),
                PreparedResult::Void | PreparedResult::Scalar(_) => None,
            })
            .collect();
        assert!(machine.release(*outer));
        assert!(children.contains(&continuation));
        for child in children {
            assert!(machine.release(child));
        }
        assert!(machine.release(*second_outer));
    }
}
