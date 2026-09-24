//! Evacuation on a prepared machine: export the graph a handle roots into a
//! detached [`Parcel`], and import a parcel from another machine as a new
//! old-space arena rooted by a fresh handle. The heap-level copier and its
//! invariants live in `tidepool_heap::gc::evacuate`; this module supplies
//! the machine's spaces, ledger and roots.
//!
//! What crosses: every object reachable from the handle that lives in this
//! machine's nursery or old space, copied; external payloads, copied into
//! parcel storage and re-registered on import; static references and code,
//! shared by address with any machine that installed the same image
//! (`CompiledProgram::shared_statics`). What never crosses: a continuation
//! or an object under evaluation (typed refusals), and identity — a MutVar
//! arrives as its own copy.

use super::machine::{PreparedHandle, PreparedMachine};
use super::run::runtime_error;
use super::ExecutionError;
use crate::suspension::RealmId;
use std::collections::HashMap;
use tidepool_heap::descriptor_region::DescriptorArena;
use tidepool_heap::execution_descriptor::DescriptorTraceError;
use tidepool_heap::external_storage::ExternalStorageKind;
use tidepool_heap::gc::evacuate::{export_reachable, MachineSpaces, NurseryView, Parcel};
use tidepool_repr::execution_schema::RuntimeRep;

impl PreparedMachine<'_> {
    /// Export the graph `handle` roots into a parcel. The machine is
    /// unchanged afterwards: every forwarding word the copy wrote is
    /// restored before this returns, on success and on failure alike.
    pub fn export_parcel(&mut self, handle: PreparedHandle) -> Result<Parcel, ExecutionError> {
        self.ensure_handle_access()?;
        let _quiescent = self.quiesce()?;
        let root = {
            let entry = self
                .handles
                .handle(handle.raw())
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            // SAFETY: the ledger keeps the slot registered while the handle
            // is live, and the machine is quiescent.
            unsafe { entry.slot.current() }
        };
        let mut state = self
            .machine
            .take_gc_state()
            .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
        let outcome: Result<Parcel, ExecutionError> = (|| {
            let used = (self.vmctx.alloc_ptr as usize)
                .checked_sub(state.active_start as usize)
                .filter(|used| *used <= state.active_size && used % 8 == 0)
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            let prepared = state
                .prepared
                .as_mut()
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            // SAFETY: quiescent machine; `used` is the initialized nursery
            // prefix and `prepared.space` pins every layout in it.
            let (nursery, nursery_externals) =
                unsafe { NurseryView::walk(&prepared.space, state.active_start, used) }
                    .map_err(ExecutionError::Evacuation)?;
            let mut arena_externals = 0;
            for arena in &self.old_space.prepared_arenas {
                arena
                    .walk_sealed(|_, descriptor| {
                        arena_externals += usize::from(descriptor.external_kind().is_some());
                        Ok(())
                    })
                    .map_err(ExecutionError::Evacuation)?;
            }
            let spaces = MachineSpaces {
                nursery,
                arenas: &self.old_space.prepared_arenas,
            };
            // SAFETY: the machine is quiescent and exclusively borrowed for
            // the whole export; the ledger describes and owns every payload.
            unsafe {
                export_reachable(
                    root as usize,
                    &spaces,
                    nursery_externals + arena_externals,
                    &mut prepared.space,
                    &self.descriptors,
                    &*self.machine,
                )
            }
            .map_err(ExecutionError::Evacuation)
        })();
        self.machine.put_gc_state(state);
        outcome
    }

    /// Import a parcel exported by another machine that installed the same
    /// images: its objects become one new old-space arena, its payloads
    /// become ledger allocations, and the returned handle roots the value in
    /// `realm`. A parcel naming a descriptor this machine has not installed
    /// is refused before anything is allocated.
    pub fn import_parcel(
        &mut self,
        mut parcel: Parcel,
        realm: RealmId,
    ) -> Result<PreparedHandle, ExecutionError> {
        self.ensure_handle_access()?;
        let _quiescent = self.quiesce()?;
        let root = parcel.root();
        if root == 0 {
            return Err(ExecutionError::Evacuation(
                DescriptorTraceError::TaggedNull { value: 0 },
            ));
        }
        for header in parcel
            .descriptor_headers()
            .map_err(ExecutionError::Evacuation)?
        {
            let known = self.descriptor_registry.contains_key(&header)
                || self
                    .descriptors
                    .iter()
                    .any(|descriptor| descriptor.initial_header_word() == header);
            if !known {
                return Err(ExecutionError::Evacuation(
                    DescriptorTraceError::UnknownDescriptor { address: header },
                ));
            }
        }
        self.handles.try_reserve_handles(1).map_err(|_| {
            runtime_error(&self.machine, crate::host_fns::RuntimeError::HeapOverflow)
        })?;

        // Payloads first: the copier expands them through this machine's
        // ledger, so the parcel must already point at ledger allocations.
        let mut payload_map = HashMap::new();
        for payload in parcel.payloads() {
            let shape = payload.shape();
            let published = match shape.kind {
                ExternalStorageKind::Bytes => self
                    .machine
                    .allocate_external_bytes(shape.logical_len, shape.align),
                ExternalStorageKind::BoxedArray => self
                    .machine
                    .allocate_external_storage(shape.kind, shape.logical_len),
            }
            .map_err(|error| {
                ExecutionError::Evacuation(DescriptorTraceError::ExternalPayload(error))
            })?;
            let data = payload.data();
            // SAFETY: the ledger allocated `logical_len` bytes (or slots)
            // after the length prefix at `published`.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), published.add(8), data.len()) };
            payload_map.insert(payload.published() as usize, published);
        }
        // SAFETY: the parcel is sealed and owned; no copy is in progress.
        unsafe { parcel.rewrite_payloads(&payload_map) }.map_err(ExecutionError::Evacuation)?;

        let relocated = if parcel.bytes() == 0 {
            // A static or otherwise stable root: nothing to copy.
            root
        } else {
            let mut state = self
                .machine
                .take_gc_state()
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            let outcome: Result<usize, ExecutionError> = (|| {
                let prepared = state
                    .prepared
                    .as_mut()
                    .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
                let mut arena =
                    DescriptorArena::reserve(parcel.bytes(), self.descriptors.iter().cloned())
                        .map_err(ExecutionError::Evacuation)?;
                self.old_space.prepared_arenas.try_reserve(1).map_err(|_| {
                    runtime_error(&self.machine, crate::host_fns::RuntimeError::HeapOverflow)
                })?;
                let range = arena.allocation_range();
                self.machine
                    .register_old_space_arena(range.start as *const u8, range.end as *const u8);
                self.machine.arm_write_barrier();
                // SAFETY: quiescent, exclusively borrowed machine; the parcel
                // already points at this ledger's payloads.
                let copied = unsafe {
                    parcel.import_into(&mut arena, &mut prepared.space, None, &*self.machine)
                };
                let (relocated, _) = match copied {
                    Ok(copied) => copied,
                    Err(error) => {
                        self.machine.retire_old_space_arena(
                            range.start as *const u8,
                            range.end as *const u8,
                        );
                        return Err(ExecutionError::Evacuation(error));
                    }
                };
                let visited: Vec<_> = prepared.space.visited_external_payloads().collect();
                self.old_space.prepared_arenas.push(arena);
                self.machine
                    .retain_external_payloads(&visited)
                    .map_err(|error| {
                        ExecutionError::Evacuation(DescriptorTraceError::ExternalPayload(error))
                    })?;
                Ok(relocated)
            })();
            self.machine.put_gc_state(state);
            outcome?
        };
        let slot = self
            .old_space
            .adopt_root(&self.machine, relocated as *mut u8)
            .map_err(|cause| runtime_error(&self.machine, cause))?;
        let raw = self
            .handles
            .insert_handle(slot, realm, RuntimeRep::LiftedRef);
        Ok(PreparedHandle::new(raw, RuntimeRep::LiftedRef))
    }
}
