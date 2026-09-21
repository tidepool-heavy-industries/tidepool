//! Rooted observation may execute generated code; no heap borrow crosses it.
//!
//! The invocation keeps its active buffer installed until observation ends.
//! Expand via `recursion::try_expand_and_collapse`, seeding ObservationSlots:
//! admit the current managed pointer, force lifted values, then obtain the
//! current range/cursor again before reading fields. Snapshot every child's
//! logical scalar/reference and register managed slots before the next force.
//! Constructor identity/field reps come only from the owner's descriptor map.
//! A shared budget covers value nodes and copied payload bytes; unknown/function/PAP
//! shapes are typed observation failures, and an exhausted budget is one too
//! unless the caller asked for a bounded walk, which cuts and marks the cut
//! (`observation::BudgetPolicy`). Cancellation
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
use tidepool_bridge::HaskellValue;
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

/// One position on the observation frontier: a rooted slot still to be read,
/// or a point a BOUNDED walk already cut. A cut carries no slot, so the walk
/// neither roots nor forces anything beyond the budget it has spent.
#[derive(Clone, Copy)]
enum Frontier {
    Slot(ObservationSlot),
    Cut,
}

/// Whether `error` is only this observation's budget running out — the one
/// failure a bounded walk answers with a cut instead of propagating.
fn is_budget_exhaustion(error: &ExecutionError) -> bool {
    matches!(
        error,
        ExecutionError::Observation(ObservationFailure::BudgetExceeded { .. })
    )
}

/// Materialize results while the invocation nursery is still installed. Each
/// generated force may copy that nursery, so the heap reader is reconstructed
/// only after the force returns and is dropped before the next force begins.
///
/// `policy` chooses what an exhausted budget means:
/// [`crate::observation::BudgetPolicy::Complete`] fails the whole observation,
/// which is what every caller asked for before a bounded one existed;
/// `Bounded` stops the walk there and leaves
/// [`crate::observation::oversize_cut`] in place of the subtree it did not
/// read. A bounded walk also stops FORCING at that point: a value it cannot
/// materialize is not worth evaluating, and the retained handle keeps it
/// reachable for a later, smaller look.
#[expect(
    clippy::too_many_arguments,
    reason = "observation independently borrows machine and program custody, VM context, static and old heaps, descriptor roots, seeds, budget, and cut policy"
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
    policy: crate::observation::BudgetPolicy,
) -> Result<Vec<HaskellValue>, ExecutionError> {
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
        let expanded = recursion::try_expand_and_collapse::<
            super::observe::ObservationFrame<recursion::PartiallyApplied>,
            _,
            _,
            _,
        >(
            Frontier::Slot(root),
            |frontier| {
                let slot = match frontier {
                    // Already cut: emit the marker without reading, rooting or
                    // forcing anything.
                    Frontier::Cut => {
                        return Ok(super::observe::ObservationFrame::Leaf(
                            crate::observation::oversize_cut(),
                        ))
                    }
                    Frontier::Slot(slot) => slot,
                };
                if policy.cuts() && observation_budget.remaining == 0 {
                    return Ok(super::observe::ObservationFrame::Leaf(
                        crate::observation::oversize_cut(),
                    ));
                }
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
                let frame = heap.expand(seed, &mut observation_budget, policy)?;
                let mapped: Result<super::observe::ObservationFrame<Frontier>, ExecutionError> =
                    match frame {
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
                                // The root frontier is itself budget-sized, so a
                                // bounded walk can run out of SLOTS before it runs
                                // out of budget. That is the same exhaustion and
                                // gets the same cut.
                                slots.push(match roots.push(seed.word, seed.rep) {
                                    Ok(slot) => Frontier::Slot(slot),
                                    Err(error) if policy.cuts() && is_budget_exhaustion(&error) => {
                                        Frontier::Cut
                                    }
                                    Err(error) => return Err(error),
                                });
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
                    Ok(HaskellValue::Con(identity, fields))
                }
            },
        );
        let value = match expanded {
            Ok(value) => value,
            Err(ExecutionError::Runtime(failure))
                if failure.cause == RuntimeError::RaisedException =>
            {
                // The observation's own roots stay registered while the
                // exception is described; they are released on return.
                describe_raised_exception(machine, program, vmctx, statics, registry, old_space);
                return Err(super::run::runtime_error_from_machine(machine));
            }
            Err(error) => return Err(error),
        };
        values.push(value);
    }
    Ok(values)
}

/// Node budget for describing one raised exception.
const EXCEPTION_DESCRIPTION_BUDGET: usize = 16_384;

/// After a call failed by raising a Haskell exception, force the exception
/// under a bounded budget and restore the failure with its message (the
/// `error` text, or the first string the exception carries). Any failure
/// while describing leaves the plain raise.
pub(super) fn describe_raised_exception(
    machine: &MachineState,
    program: &CompiledProgram,
    vmctx: &mut VMContext,
    statics: &[Arc<StaticRegion>],
    registry: &BTreeMap<usize, super::DescriptorMetadata>,
    old_space: &OldSpace,
) {
    let Some(word) = machine.suspend_prepared_raise() else {
        return;
    };
    let scope = if unsafe { machine.prepared_old_space() }.is_some() {
        None
    } else {
        super::roots::OldSpaceScope::new(machine, old_space).ok()
    };
    let observed = observe_results(
        machine,
        program,
        vmctx,
        statics,
        registry,
        old_space,
        &[super::observe::ObservationSeed {
            word,
            rep: RuntimeRep::LiftedRef,
        }],
        EXCEPTION_DESCRIPTION_BUDGET,
        crate::observation::BudgetPolicy::Complete,
    );
    drop(scope);
    let message = observed
        .ok()
        .and_then(|values| values.first().and_then(exception_message));
    machine.restore_prepared_raise(message);
}

/// The message an observed exception carries: the first character list,
/// searching the exception's own fields before its context.
fn exception_message(exception: &HaskellValue) -> Option<String> {
    let mut pending: Vec<&HaskellValue> = match exception {
        HaskellValue::Con(_, fields) => fields.iter().collect(),
        other => vec![other],
    };
    while let Some(value) = pending.pop() {
        if let Some(text) = character_list(value) {
            return Some(text);
        }
        if let HaskellValue::Con(_, fields) = value {
            pending.extend(fields.iter().rev());
        }
    }
    None
}

fn character_list(mut value: &HaskellValue) -> Option<String> {
    let mut text = String::new();
    loop {
        match value {
            HaskellValue::Con(_, fields) if fields.is_empty() => {
                return (!text.is_empty()).then_some(text);
            }
            HaskellValue::Con(_, fields) if fields.len() == 2 => {
                text.push(character(&fields[0])?);
                value = &fields[1];
            }
            _ => return None,
        }
    }
}

/// A boxed character; observation reads `Char#` as its 32-bit word.
fn character(value: &HaskellValue) -> Option<char> {
    match value {
        HaskellValue::Lit(tidepool_repr::Literal::LitChar(c)) => Some(*c),
        HaskellValue::Lit(tidepool_repr::Literal::LitWord(word)) => {
            u32::try_from(*word).ok().and_then(char::from_u32)
        }
        HaskellValue::Con(_, fields) if fields.len() == 1 => character(&fields[0]),
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "heap reconstruction independently borrows machine, VM and heap regions plus mutable generation-index custody"
)]
pub(super) fn current_heap<'a>(
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

#[cfg(test)]
mod tests {
    use super::exception_message;
    use tidepool_bridge::HaskellValue;
    use tidepool_repr::{DataConId, Literal};

    fn text(s: &str, boxed_word: bool) -> HaskellValue {
        s.chars()
            .rev()
            .fold(HaskellValue::Con(DataConId(1), vec![]), |tail, c| {
                let raw = if boxed_word {
                    HaskellValue::Lit(Literal::LitWord(u64::from(c)))
                } else {
                    HaskellValue::Lit(Literal::LitChar(c))
                };
                HaskellValue::Con(
                    DataConId(2),
                    vec![HaskellValue::Con(DataConId(3), vec![raw]), tail],
                )
            })
    }

    #[test]
    fn exception_message_prefers_the_exception_over_its_context() {
        let exception = HaskellValue::Con(
            DataConId(4),
            vec![
                HaskellValue::Con(DataConId(5), vec![text("backtrace frame", true)]),
                HaskellValue::Con(DataConId(6), vec![text("failed suffix", true)]),
            ],
        );
        assert_eq!(
            exception_message(&exception).as_deref(),
            Some("failed suffix")
        );
    }

    #[test]
    fn exception_message_is_absent_without_text() {
        let exception = HaskellValue::Con(
            DataConId(4),
            vec![
                HaskellValue::Con(DataConId(1), vec![]),
                HaskellValue::Lit(Literal::LitWord(7)),
            ],
        );
        assert_eq!(exception_message(&exception), None);
        assert_eq!(
            exception_message(&HaskellValue::Con(DataConId(4), vec![text("é", false)])).as_deref(),
            Some("é")
        );
    }
}
