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
    chunk: usize,
    slot: usize,
}

pub(super) const ROOT_CHUNK_WORDS: usize = 64;

struct RootChunk {
    words: RootWords,
    active: Box<[bool]>,
    registered: Box<[bool]>,
}

impl RootChunk {
    fn new() -> Result<Self, RuntimeError> {
        Ok(Self {
            words: RootWords::new(ROOT_CHUNK_WORDS).map_err(|_| RuntimeError::HeapOverflow)?,
            active: vec![false; ROOT_CHUNK_WORDS].into_boxed_slice(),
            registered: vec![false; ROOT_CHUNK_WORDS].into_boxed_slice(),
        })
    }
}

pub(super) struct ConstructionCore {
    owner: u64,
    roots: Vec<RootChunk>,
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
        let chunk = match self
            .roots
            .iter()
            .position(|chunk| chunk.active.iter().any(|active| !active))
        {
            Some(chunk) => chunk,
            None => {
                self.roots
                    .try_reserve(1)
                    .map_err(|_| RuntimeError::HeapOverflow)?;
                self.roots.push(RootChunk::new()?);
                self.roots.len() - 1
            }
        };
        let slot_index = self.roots[chunk]
            .active
            .iter()
            .position(|active| !active)
            .ok_or(RuntimeError::BadPointer)?;
        self.roots[chunk].active[slot_index] = true;
        let slot = self.roots[chunk]
            .words
            .slot_address(slot_index)
            .ok_or(RuntimeError::BadPointer)?;
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            // Register the zero-filled slot before any collection or object
            // publication. It is already at its final address, and a failed
            // operation may safely leave it registered until the owner drops.
            machine.register_rust_root(slot);
            self.roots[chunk].registered[slot_index] = true;
        }
        Ok(PreparedRoot {
            chunk,
            slot: slot_index,
        })
    }

    fn publish_root(&self, root: PreparedRoot, word: usize) -> ConstructionNode {
        // `prepare_root` proved the index and allocated its one word. No
        // fallible root work remains after the nursery object is committed.
        unsafe {
            self.roots[root.chunk]
                .words
                .as_mut_ptr()
                .add(root.slot)
                .write(word as u64)
        };
        ConstructionNode {
            index: root.chunk * ROOT_CHUNK_WORDS + root.slot,
            owner: self.owner,
        }
    }

    fn root(&self, node: ConstructionNode) -> Result<(&RootChunk, usize), NodeAccessError> {
        if node.owner != self.owner {
            return Err(NodeAccessError::Foreign);
        }
        let chunk = node.index / ROOT_CHUNK_WORDS;
        let slot = node.index % ROOT_CHUNK_WORDS;
        let root = self.roots.get(chunk).ok_or(NodeAccessError::Invalid)?;
        root.active
            .get(slot)
            .filter(|active| **active)
            .ok_or(NodeAccessError::Invalid)?;
        Ok((root, slot))
    }

    pub(super) fn consume(
        &mut self,
        machine: &MachineState,
        node: ConstructionNode,
    ) -> Result<(), NodeAccessError> {
        let chunk_index = node.index / ROOT_CHUNK_WORDS;
        let slot = node.index % ROOT_CHUNK_WORDS;
        self.root(node)?;
        let root = &mut self.roots[chunk_index];
        if root.registered[slot] {
            let address = root
                .words
                .slot_address(slot)
                .ok_or(NodeAccessError::Invalid)?;
            machine.deregister_rust_root(address);
            root.registered[slot] = false;
        }
        root.active[slot] = false;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn root_metrics(&self) -> (usize, usize) {
        (
            self.roots.len(),
            self.roots
                .iter()
                .map(|chunk| chunk.active.iter().filter(|active| **active).count())
                .sum(),
        )
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
        let (root, slot) = self.root(node)?;
        root.words
            .read(slot)
            .map(|word| word as usize)
            .map_err(|_| NodeAccessError::Invalid)
    }

    pub(super) fn slot(&self, node: ConstructionNode) -> Result<*mut u64, NodeAccessError> {
        if node.owner != self.owner {
            return Err(NodeAccessError::Foreign);
        }
        let (root, slot) = self.root(node)?;
        root.words
            .slot_address(slot)
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
        consumed: &[ConstructionNode],
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
        let node = self.publish_root(root, pointer as usize | usize::from(descriptor.tag()));
        for child in consumed {
            self.consume(machine, *child)
                .map_err(|_| ConstructionError::Runtime(RuntimeError::BadPointer))?;
        }
        Ok(node)
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
