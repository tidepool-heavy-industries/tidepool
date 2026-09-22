//! Shared transactional mechanics for managed values built by Rust.
//!
//! An operation allocates and registers its fixed root slot before it can
//! collect, allocate external storage, or publish a nursery object. Constructor
//! fields are resolved only after the capacity check, so a collection can never
//! leave a stale child pointer in a caller-owned buffer.

use super::roots::RootWords;
use crate::{
    context::VMContext,
    descriptor_bridge::{marshal_descriptor_object, DescriptorMarshalError, DescriptorValue},
    host_fns::RuntimeError,
    machine_state::MachineState,
};
use tidepool_heap::{
    execution_descriptor::ObjectDescriptor,
    external_storage::{ExternalStorageKind, ExternalStorageValidationError},
};
use tidepool_repr::execution_schema::RuntimeRep;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConstructionNode {
    index: usize,
    owner: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NodeAccessError {
    Foreign,
    Invalid,
}

#[derive(Debug)]
pub(super) enum ConstructionError<E> {
    Runtime(RuntimeError),
    Operation(E),
    TooLarge(usize),
    Marshal(DescriptorMarshalError),
    Storage(ExternalStorageValidationError),
}

struct PreparedRoot {
    index: usize,
}

pub(super) struct ConstructionCore {
    owner: u64,
    roots: Vec<RootWords>,
    roots_mark: usize,
}

impl ConstructionCore {
    pub(super) fn new(machine: &MachineState, owner: u64) -> Self {
        Self {
            owner,
            roots: Vec::new(),
            roots_mark: machine.rust_roots_len(),
        }
    }

    pub(super) fn release(&mut self, machine: &MachineState) {
        machine.truncate_rust_roots(self.roots_mark);
    }

    fn prepare_root(
        &mut self,
        machine: &MachineState,
        rep: RuntimeRep,
    ) -> Result<PreparedRoot, RuntimeError> {
        self.roots
            .try_reserve(1)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        let root = RootWords::new(1).map_err(|_| RuntimeError::HeapOverflow)?;
        let slot = root.slot_address(0).ok_or(RuntimeError::BadPointer)?;
        self.roots.push(root);
        let index = self.roots.len() - 1;
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            // Register the zero-filled slot before any collection or object
            // publication. It is already at its final address, and a failed
            // operation may safely leave it registered until the owner drops.
            machine.register_rust_root(slot);
        }
        Ok(PreparedRoot { index })
    }

    fn publish_root(&self, root: PreparedRoot, word: usize) -> ConstructionNode {
        // `prepare_root` proved the index and allocated its one word. No
        // fallible root work remains after the nursery object is committed.
        unsafe { self.roots[root.index].as_mut_ptr().write(word as u64) };
        ConstructionNode {
            index: root.index,
            owner: self.owner,
        }
    }

    fn ensure_capacity<E>(
        machine: &MachineState,
        vmctx: &mut VMContext,
        extent: usize,
        collect: impl FnOnce(&MachineState, &mut VMContext, usize) -> Result<(), E>,
    ) -> Result<(), ConstructionError<E>> {
        let free = (vmctx.alloc_limit as usize).saturating_sub(vmctx.alloc_ptr as usize);
        if free < extent {
            collect(machine, vmctx, extent).map_err(ConstructionError::Operation)?;
            let free = (vmctx.alloc_limit as usize).saturating_sub(vmctx.alloc_ptr as usize);
            if free < extent {
                return Err(ConstructionError::TooLarge(extent));
            }
        }
        Ok(())
    }

    pub(super) fn push_word(
        &mut self,
        machine: &MachineState,
        word: usize,
        rep: RuntimeRep,
    ) -> Result<ConstructionNode, RuntimeError> {
        let root = self.prepare_root(machine, rep)?;
        Ok(self.publish_root(root, word))
    }

    pub(super) fn word(&self, node: ConstructionNode) -> Result<usize, NodeAccessError> {
        if node.owner != self.owner {
            return Err(NodeAccessError::Foreign);
        }
        self.roots
            .get(node.index)
            .ok_or(NodeAccessError::Invalid)?
            .read(0)
            .map(|word| word as usize)
            .map_err(|_| NodeAccessError::Invalid)
    }

    pub(super) fn slot(&self, node: ConstructionNode) -> Result<*mut u64, NodeAccessError> {
        if node.owner != self.owner {
            return Err(NodeAccessError::Foreign);
        }
        self.roots
            .get(node.index)
            .and_then(|root| root.slot_address(0))
            .map(|slot| slot.cast::<u64>())
            .ok_or(NodeAccessError::Invalid)
    }

    /// Build one constructor. `resolve` runs after the last possible
    /// collection and writes into storage allocated before that collection.
    pub(super) fn constructor<E>(
        &mut self,
        machine: &MachineState,
        vmctx: &mut VMContext,
        descriptor: &ObjectDescriptor,
        field_count: usize,
        collect: impl FnOnce(&MachineState, &mut VMContext, usize) -> Result<(), E>,
        resolve: impl FnOnce(&Self, &mut [DescriptorValue]) -> Result<(), E>,
    ) -> Result<ConstructionNode, ConstructionError<E>> {
        let root = self
            .prepare_root(machine, RuntimeRep::LiftedRef)
            .map_err(ConstructionError::Runtime)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(field_count)
            .map_err(|_| ConstructionError::Runtime(RuntimeError::HeapOverflow))?;
        values.resize(field_count, DescriptorValue::Bits([0; 16]));
        let extent = (descriptor.allocation_extent() as usize).next_multiple_of(8);
        Self::ensure_capacity(machine, vmctx, extent, collect)?;
        resolve(self, &mut values).map_err(ConstructionError::Operation)?;
        let pointer = vmctx.alloc_ptr;
        unsafe { marshal_descriptor_object(pointer, extent, descriptor, &values) }
            .map_err(ConstructionError::Marshal)?;
        vmctx.alloc_ptr = unsafe { pointer.add(extent) };
        Ok(self.publish_root(root, pointer as usize | usize::from(descriptor.tag())))
    }

    /// Allocate and initialize an external byte payload and its managed
    /// wrapper as one transaction. Until the wrapper is published, failures
    /// release the payload explicitly.
    pub(super) fn bytes<E>(
        &mut self,
        machine: &MachineState,
        vmctx: &mut VMContext,
        descriptor: &ObjectDescriptor,
        bytes: &[u8],
        collect: impl FnOnce(&MachineState, &mut VMContext, usize) -> Result<(), E>,
    ) -> Result<ConstructionNode, ConstructionError<E>> {
        let root = self
            .prepare_root(machine, RuntimeRep::LiftedRef)
            .map_err(ConstructionError::Runtime)?;
        let extent = (descriptor.allocation_extent() as usize).next_multiple_of(8);
        Self::ensure_capacity(machine, vmctx, extent, collect)?;
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, bytes.len())
            .map_err(ConstructionError::Storage)?;
        if let Err(error) = machine.store_external_bytes(payload, 0, bytes) {
            if !machine.release_external_storage(payload) {
                return Err(ConstructionError::Runtime(RuntimeError::BadPointer));
            }
            return Err(ConstructionError::Storage(error));
        }
        let pointer = vmctx.alloc_ptr;
        if let Err(error) = unsafe {
            marshal_descriptor_object(
                pointer,
                extent,
                descriptor,
                &[DescriptorValue::Address(payload.cast_const())],
            )
        } {
            if !machine.release_external_storage(payload) {
                return Err(ConstructionError::Runtime(RuntimeError::BadPointer));
            }
            return Err(ConstructionError::Marshal(error));
        }
        vmctx.alloc_ptr = unsafe { pointer.add(extent) };
        Ok(self.publish_root(root, pointer as usize | usize::from(descriptor.tag())))
    }
}
