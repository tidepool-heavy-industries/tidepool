//! Rooted observation may execute generated code; no heap borrow crosses it.
//!
//! The invocation keeps its active buffer installed until observation ends.
//! Expand via `recursion::try_expand_and_collapse`, seeding ObservationSlots:
//! admit the current managed pointer, force lifted values, then obtain the
//! current range/cursor again before reading fields. Snapshot every child's
//! logical scalar/reference and register managed slots before the next force.
//! Constructor identity/field reps come only from the owner's descriptor map.
//! A shared budget covers value nodes and copied payload bytes; unknown/function/PAP
//! shapes and exhausted budget are typed observation failures. Cancellation
//! governs evaluation inside force (a node budget cannot bound a diverging body).
//!
//! Exact-start indexing is observation-local, not a per-allocation registry:
//! rebuild after a changed GC generation; otherwise append newly initialized
//! objects up to the current cursor. Never keep a borrowed nursery slice across
//! `force`. Keep the same physical/representation reader as nonforcing
//! observation, not a second decoder. Release root slots before their buffers.

use super::{CompiledProgram, ExecutionError, ObservationFailure};
use crate::context::VMContext;
use crate::host_fns::RuntimeError;
use crate::machine_state::MachineState;
use crate::old_space::OldSpace;
use crate::prepared_control::CallStatus;
use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_bridge::Value;
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::RuntimeRep;

#[derive(Clone, Copy)]
pub(super) struct ObservationSlot {
    pub index: usize,
    pub rep: RuntimeRep,
}

/// Stable storage for the bounded observation frontier. Allocate once before
/// registering any address; never grow or move it until roots are truncated.
/// Each expanded constructor snapshots and roots all children before forcing
/// any child, so later traversal reads relocated slots, not stale parent fields.
pub(super) struct ObservationRoots<'a> {
    machine: &'a MachineState,
    slots: Box<[UnsafeCell<usize>]>,
    used: usize,
    mark: usize,
    stack: super::safepoint::NativeStackBounds,
}

impl<'a> ObservationRoots<'a> {
    pub fn new(machine: &'a MachineState, budget: usize) -> Result<Self, ExecutionError> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(budget)
            .map_err(|_| super::run::runtime_error(machine, RuntimeError::HeapOverflow))?;
        slots.resize_with(budget, || UnsafeCell::new(0));
        let stack = super::safepoint::NativeStackBounds::current()
            .map_err(|cause| super::run::runtime_error(machine, cause))?;
        Ok(Self {
            machine,
            slots: slots.into_boxed_slice(),
            used: 0,
            mark: machine.rust_roots_len(),
            stack,
        })
    }

    pub fn push(
        &mut self,
        word: usize,
        rep: RuntimeRep,
    ) -> Result<ObservationSlot, ExecutionError> {
        if self.used == self.slots.len() {
            return Err(ObservationFailure::BudgetExceeded {
                limit: self.slots.len(),
            }
            .into());
        }
        let index = self.used;
        self.used += 1;
        // The collector updates this slot through its registered raw address
        // while generated code is running; UnsafeCell makes that interior
        // mutation explicit to Rust's aliasing model.
        unsafe { self.slots[index].get().write(word) };
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            // The fixed allocation outlives every registered root; Drop removes
            // addresses before deallocating its backing storage, including unwind.
            self.machine
                .register_rust_root(self.slots[index].get().cast::<*mut u8>());
        }
        Ok(ObservationSlot { index, rep })
    }

    pub fn read(&self, slot: ObservationSlot) -> usize {
        unsafe { *self.slots[slot.index].get() }
    }

    /// Only the program-generated platform adapter crosses from Rust to Tail.
    /// Status is inspected before any returned word. A failed force leaves the
    /// original slot registered for unwind; terminal failure reads no heap.
    pub fn force(
        &mut self,
        slot: ObservationSlot,
        program: &CompiledProgram,
        vmctx: &mut VMContext,
    ) -> Result<(), ExecutionError> {
        if self.machine.prepared_call_status() != CallStatus::Success {
            return Err(super::run::runtime_error_from_machine(self.machine));
        }
        if slot.rep != RuntimeRep::LiftedRef {
            return Ok(());
        }
        let reserve = program
            .pipeline
            .native_frame_maximum()
            .checked_mul(2)
            .ok_or_else(|| super::run::runtime_error(self.machine, RuntimeError::StackOverflow))?;
        self.stack
            .ensure_current_frame_reserve(reserve)
            .map_err(|cause| super::run::runtime_error(self.machine, cause))?;
        let input = self.read(slot);
        let output = self.slots[slot.index].get().cast::<u64>();
        let pointer = program
            .pipeline
            .get_function_ptr(program.prepared_force_adapter());
        // Compiled owner pins both platform adapter and every Tail target.
        let adapter: unsafe extern "C" fn(*mut VMContext, *mut u64, usize) -> i32 =
            unsafe { std::mem::transmute(pointer) };
        let raw = unsafe { adapter(vmctx, output, input) };
        let status = CallStatus::from_raw(i64::from(raw))
            .map_err(|_| super::run::runtime_error(self.machine, RuntimeError::BadPointer))?;
        if status != CallStatus::Success
            || self.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(super::run::runtime_error_for_status(self.machine, status));
        }
        Ok(())
    }
}

/// Materialize results while the invocation nursery is still installed. Each
/// generated force may copy that nursery, so the heap reader is reconstructed
/// only after the force returns and is dropped before the next force begins.
#[expect(
    clippy::too_many_arguments,
    reason = "observation independently borrows machine and program custody, VM context, static and old heaps, descriptor roots, seeds, and budget"
)]
pub(super) fn observe_results(
    machine: &MachineState,
    program: &CompiledProgram,
    vmctx: &mut VMContext,
    statics: &[Arc<StaticRegion>],
    registry: &BTreeMap<usize, super::DescriptorMetadata>,
    old_space: &OldSpace,
    seeds: &[super::observe::ObservationSeed],
    budget: usize,
) -> Result<Vec<Value>, ExecutionError> {
    let mut roots = ObservationRoots::new(machine, budget)?;
    let mut result_slots = Vec::new();
    result_slots
        .try_reserve_exact(seeds.len())
        .map_err(|_| super::run::runtime_error(machine, RuntimeError::HeapOverflow))?;
    for seed in seeds {
        result_slots.push(roots.push(seed.word, seed.rep)?);
    }

    let mut observation_budget = super::observe::ObservationBudget {
        remaining: budget,
        limit: budget,
    };
    let mut starts = Vec::new();
    let mut scanned_words = 0;
    let mut indexed_generation = machine.gc_generation();
    let mut values = Vec::new();
    values
        .try_reserve_exact(result_slots.len())
        .map_err(|_| super::run::runtime_error(machine, RuntimeError::HeapOverflow))?;
    for root in result_slots {
        let value = recursion::try_expand_and_collapse::<
            super::observe::ObservationFrame<recursion::PartiallyApplied>,
            _,
            _,
            _,
        >(
            root,
            |slot| {
                if matches!(slot.rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
                    let heap = current_heap(
                        machine,
                        vmctx,
                        statics,
                        registry,
                        old_space,
                        &mut starts,
                        &mut scanned_words,
                        &mut indexed_generation,
                    )?;
                    // Admission must happen before generated code can move or
                    // inspect the pointer. The heap borrow ends before force.
                    heap.validate_reference(roots.read(slot))?;
                }
                roots.force(slot, program, vmctx)?;
                let seed = super::observe::ObservationSeed {
                    word: roots.read(slot),
                    rep: slot.rep,
                };
                // The reader borrows the active prefix only for this call.
                // `expand` snapshots scalar/reference words, and no borrowed
                // heap storage escapes into the recursive worklist.
                let heap = current_heap(
                    machine,
                    vmctx,
                    statics,
                    registry,
                    old_space,
                    &mut starts,
                    &mut scanned_words,
                    &mut indexed_generation,
                )?;
                let frame = heap.expand(seed, &mut observation_budget)?;
                let mapped: Result<
                    super::observe::ObservationFrame<ObservationSlot>,
                    ExecutionError,
                > = match frame {
                    super::observe::ObservationFrame::Leaf(value) => {
                        Ok(super::observe::ObservationFrame::Leaf(value))
                    }
                    super::observe::ObservationFrame::Constructor(identity, fields) => {
                        let mut slots = Vec::new();
                        slots
                            .try_reserve_exact(fields.len())
                            .map_err(|_| ObservationFailure::Integrity(
                                tidepool_heap::execution_descriptor::DescriptorTraceError::MetadataAllocation,
                            ))?;
                        for seed in fields {
                            slots.push(roots.push(seed.word, seed.rep)?);
                        }
                        Ok(super::observe::ObservationFrame::Constructor(
                            identity, slots,
                        ))
                    }
                };
                mapped
            },
            |frame| match frame {
                super::observe::ObservationFrame::Leaf(value) => Ok(value),
                super::observe::ObservationFrame::Constructor(identity, fields) => {
                    let mut fields = fields;
                    fields.reverse();
                    Ok(Value::Con(identity, fields))
                }
            },
        )?;
        values.push(value);
    }
    Ok(values)
}

#[expect(
    clippy::too_many_arguments,
    reason = "heap reconstruction independently borrows machine, VM and heap regions plus mutable generation-index custody"
)]
fn current_heap<'a>(
    machine: &'a MachineState,
    vmctx: &VMContext,
    statics: &'a [Arc<StaticRegion>],
    registry: &'a BTreeMap<usize, super::DescriptorMetadata>,
    old_space: &'a OldSpace,
    starts: &mut Vec<u64>,
    scanned_words: &mut usize,
    indexed_generation: &mut u64,
) -> Result<super::observe::ObservationHeap<'a>, ExecutionError> {
    let (start, size) = machine
        .gc_active_range()
        .ok_or_else(|| super::run::runtime_error(machine, RuntimeError::BadPointer))?;
    let cursor = (vmctx.alloc_ptr as usize)
        .checked_sub(start as usize)
        .filter(|cursor| *cursor <= size && *cursor % std::mem::size_of::<u64>() == 0)
        .ok_or_else(|| super::run::runtime_error(machine, RuntimeError::BadPointer))?;
    let words = cursor / std::mem::size_of::<u64>();
    // The machine owns the active buffer until run cleanup. This borrow is
    // deliberately confined to `ObservationHeap::expand`; no force occurs
    // while the returned heap reader is live.
    let nursery = unsafe { std::slice::from_raw_parts(start.cast::<u64>(), words) };
    let generation = machine.gc_generation();
    if generation != *indexed_generation {
        starts.clear();
        *scanned_words = 0;
        *indexed_generation = generation;
    }
    super::observe::append_exact_starts(nursery, registry, starts, scanned_words)
        .map_err(ExecutionError::from)?;
    super::observe::ObservationHeap::new_with_registry_and_starts(
        nursery,
        statics,
        registry,
        starts,
        Some(old_space),
        machine,
    )
    .map_err(ExecutionError::from)
}

impl Drop for ObservationRoots<'_> {
    fn drop(&mut self) {
        self.machine.truncate_rust_roots(self.mark);
    }
}
