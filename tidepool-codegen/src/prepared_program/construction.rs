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
    generation: u64,
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

#[derive(Clone, Copy)]
struct PreparedRoot {
    node: ConstructionNode,
}

pub(super) const ROOT_CHUNK_WORDS: usize = 64;

/// Counts temporary root-slot ownership transitions. These are test-only
/// operation counts, rather than timing measurements, so wide traversal tests
/// can assert that registration and release work stays structurally bounded.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RootOperationMetrics {
    pub(super) registrations: usize,
    pub(super) releases: usize,
}

struct RootChunk {
    words: RootWords,
    active: Box<[bool]>,
    registrations: Box<[Option<usize>]>,
    generations: Box<[u64]>,
}

impl RootChunk {
    fn new() -> Result<Self, RuntimeError> {
        Ok(Self {
            words: RootWords::new(ROOT_CHUNK_WORDS).map_err(|_| RuntimeError::HeapOverflow)?,
            active: vec![false; ROOT_CHUNK_WORDS].into_boxed_slice(),
            registrations: vec![None; ROOT_CHUNK_WORDS].into_boxed_slice(),
            generations: vec![0; ROOT_CHUNK_WORDS].into_boxed_slice(),
        })
    }
}

pub(super) struct ConstructionCore {
    owner: u64,
    roots: Vec<RootChunk>,
    free: Vec<usize>,
    next_index: usize,
    #[cfg(test)]
    root_operations: RootOperationMetrics,
}

impl ConstructionCore {
    pub(super) fn new(owner: u64) -> Self {
        Self {
            owner,
            roots: Vec::new(),
            free: Vec::new(),
            next_index: 0,
            #[cfg(test)]
            root_operations: RootOperationMetrics::default(),
        }
    }

    /// Reserve enough capacity for teardown before publishing a new root
    /// chunk. `release` and `consume` then only push into this free list and
    /// cannot allocate while an error is unwinding. Growing geometrically
    /// avoids repeatedly asking the allocator for one chunk at a time.
    fn reserve_free_slots(&mut self, required: usize) -> Result<(), RuntimeError> {
        if self.free.capacity() >= required {
            return Ok(());
        }
        let mut target = self.free.capacity().max(ROOT_CHUNK_WORDS);
        while target < required {
            target = target.checked_mul(2).ok_or(RuntimeError::HeapOverflow)?;
        }
        let additional = target
            .checked_sub(self.free.len())
            .ok_or(RuntimeError::HeapOverflow)?;
        self.free
            .try_reserve_exact(additional)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        debug_assert!(self.free.capacity() >= required);
        Ok(())
    }

    pub(super) fn release(&mut self, machine: &MachineState) {
        // The free list reserves one word per admitted slot, so teardown can
        // reuse it without allocating while unwinding a failed operation.
        self.free.clear();
        for chunk in &mut self.roots {
            for slot in 0..ROOT_CHUNK_WORDS {
                if let Some(registration) = chunk.registrations[slot].take() {
                    debug_assert!(self.free.len() < self.free.capacity());
                    self.free.push(registration);
                }
            }
        }
        machine.deregister_rust_roots(&self.free);
    }

    fn prepare_root(
        &mut self,
        machine: &MachineState,
        rep: RuntimeRep,
    ) -> Result<PreparedRoot, RuntimeError> {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                let index = self.next_index;
                let chunk = index / ROOT_CHUNK_WORDS;
                if chunk == self.roots.len() {
                    debug_assert_eq!(index % ROOT_CHUNK_WORDS, 0);
                    self.roots
                        .try_reserve(1)
                        .map_err(|_| RuntimeError::HeapOverflow)?;
                    let required_free_capacity = index
                        .checked_add(ROOT_CHUNK_WORDS)
                        .ok_or(RuntimeError::HeapOverflow)?;
                    self.reserve_free_slots(required_free_capacity)?;
                    self.roots.push(RootChunk::new()?);
                }
                self.next_index = self
                    .next_index
                    .checked_add(1)
                    .ok_or(RuntimeError::HeapOverflow)?;
                index
            }
        };
        let chunk = index / ROOT_CHUNK_WORDS;
        let slot_index = index % ROOT_CHUNK_WORDS;
        let root = self.roots.get_mut(chunk).ok_or(RuntimeError::BadPointer)?;
        if root.active[slot_index] || root.registrations[slot_index].is_some() {
            return Err(RuntimeError::BadPointer);
        }
        let generation = root.generations[slot_index]
            .checked_add(1)
            .ok_or(RuntimeError::HeapOverflow)?;
        root.generations[slot_index] = generation;
        let slot = root
            .words
            .slot_address(slot_index)
            .ok_or(RuntimeError::BadPointer)?;
        // A consumed managed node leaves its old pointer in the backing word.
        // Clear it before registration so a collection between preparation and
        // publication never treats the previous occupant as this new root.
        unsafe { slot.write(std::ptr::null_mut()) };
        root.active[slot_index] = true;
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            root.registrations[slot_index] = Some(machine.register_rust_root(slot));
        }
        #[cfg(test)]
        {
            self.root_operations.registrations += 1;
        }
        Ok(PreparedRoot {
            node: ConstructionNode {
                index,
                generation,
                owner: self.owner,
            },
        })
    }

    fn publish_root(&self, root: PreparedRoot, word: usize) -> ConstructionNode {
        // `prepare_root` proved the index and allocated its one word. No
        // fallible root work remains after the nursery object is committed.
        unsafe {
            self.roots[root.node.index / ROOT_CHUNK_WORDS]
                .words
                .as_mut_ptr()
                .add(root.node.index % ROOT_CHUNK_WORDS)
                .write(word as u64)
        };
        root.node
    }

    fn abandon_root(
        &mut self,
        machine: &MachineState,
        root: PreparedRoot,
    ) -> Result<(), RuntimeError> {
        self.consume(machine, root.node)
            .map_err(|_| RuntimeError::BadPointer)
    }

    fn root(&self, node: ConstructionNode) -> Result<(&RootChunk, usize), NodeAccessError> {
        if node.owner != self.owner {
            return Err(NodeAccessError::Foreign);
        }
        let chunk = node.index / ROOT_CHUNK_WORDS;
        let slot = node.index % ROOT_CHUNK_WORDS;
        let root = self.roots.get(chunk).ok_or(NodeAccessError::Invalid)?;
        if root.generations.get(slot).copied() != Some(node.generation) {
            return Err(NodeAccessError::Invalid);
        }
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
        if let Some(registration) = root.registrations[slot].take() {
            let address = root
                .words
                .slot_address(slot)
                .ok_or(NodeAccessError::Invalid)?;
            machine.deregister_rust_root(registration, address);
        }
        unsafe { root.words.as_mut_ptr().add(slot).write(0) };
        root.active[slot] = false;
        debug_assert!(self.free.len() < self.free.capacity());
        self.free.push(node.index);
        #[cfg(test)]
        {
            self.root_operations.releases += 1;
        }
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

    #[cfg(test)]
    pub(super) fn root_operation_metrics(&self) -> RootOperationMetrics {
        self.root_operations
    }

    #[cfg(test)]
    pub(super) fn root_pool_metrics(&self) -> (usize, usize) {
        (self.free.capacity(), self.next_index)
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
        for (index, child) in consumed.iter().enumerate() {
            self.root(*child)
                .map_err(|_| ConstructionError::Runtime(RuntimeError::BadPointer))?;
            if consumed[..index].contains(child) {
                return Err(ConstructionError::Runtime(RuntimeError::BadPointer));
            }
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(field_count)
            .map_err(|_| ConstructionError::Runtime(RuntimeError::HeapOverflow))?;
        values.resize(field_count, DescriptorValue::Bits([0; 16]));
        let root = self
            .prepare_root(machine, RuntimeRep::LiftedRef)
            .map_err(ConstructionError::Runtime)?;
        let extent = (descriptor.allocation_extent() as usize).next_multiple_of(8);
        if let Err(error) = Self::ensure_capacity(machine, vmctx, extent, collect) {
            self.abandon_root(machine, root)
                .map_err(ConstructionError::Runtime)?;
            return Err(error);
        }
        if let Err(error) = resolve(self, &mut values) {
            self.abandon_root(machine, root)
                .map_err(ConstructionError::Runtime)?;
            return Err(ConstructionError::Operation(error));
        }
        let pointer = vmctx.alloc_ptr;
        if let Err(error) =
            unsafe { marshal_descriptor_object(pointer, extent, descriptor, &values) }
        {
            self.abandon_root(machine, root)
                .map_err(ConstructionError::Runtime)?;
            return Err(ConstructionError::Marshal(error));
        }
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
        if let Err(error) = Self::ensure_capacity(machine, vmctx, extent, collect) {
            self.abandon_root(machine, root)
                .map_err(ConstructionError::Runtime)?;
            return Err(error);
        }
        let payload =
            match machine.allocate_external_storage(ExternalStorageKind::Bytes, bytes.len()) {
                Ok(payload) => payload,
                Err(error) => {
                    self.abandon_root(machine, root)
                        .map_err(ConstructionError::Runtime)?;
                    return Err(ConstructionError::Storage(error));
                }
            };
        if let Err(error) = machine.store_external_bytes(payload, 0, bytes) {
            let released = machine.release_external_storage(payload);
            self.abandon_root(machine, root)
                .map_err(ConstructionError::Runtime)?;
            if !released {
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
            let released = machine.release_external_storage(payload);
            self.abandon_root(machine, root)
                .map_err(ConstructionError::Runtime)?;
            if !released {
                return Err(ConstructionError::Runtime(RuntimeError::BadPointer));
            }
            return Err(ConstructionError::Marshal(error));
        }
        vmctx.alloc_ptr = unsafe { pointer.add(extent) };
        Ok(self.publish_root(root, pointer as usize | usize::from(descriptor.tag())))
    }
}
