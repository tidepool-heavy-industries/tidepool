//! Fixed-address root storage shared by one-shot test harnesses and machines.

use super::run::{runtime_error, runtime_error_without_machine};
use super::ExecutionError;
use crate::host_fns::RuntimeError;
use crate::machine_state::MachineState;
use crate::old_space::OldSpace;
use std::cell::UnsafeCell;

/// Borrows the old-space admission owner only while native code can collect.
pub(super) struct OldSpaceScope<'a> {
    machine: &'a MachineState,
    _owner: &'a OldSpace,
}

impl<'a> OldSpaceScope<'a> {
    pub(crate) fn new(
        machine: &'a MachineState,
        owner: &'a OldSpace,
    ) -> Result<Self, ExecutionError> {
        if unsafe { machine.prepared_old_space() }.is_some() {
            return Err(runtime_error(machine, crate::host_fns::bad_pointer()));
        }
        unsafe { machine.install_prepared_old_space(owner) };
        Ok(Self {
            machine,
            _owner: owner,
        })
    }
}

impl Drop for OldSpaceScope<'_> {
    fn drop(&mut self) {
        self.machine.clear_prepared_old_space();
    }
}

/// Fixed-address, collector-updated word slots.
pub(crate) struct RootWords(Vec<UnsafeCell<u64>>);

/// A compiled image's position in every machine's root-block table.
/// Minted once per compile from a process-wide counter, so the same image
/// names the same slot on every machine that installs it; generated code
/// embeds the slot, never a block address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ImageSlot(u32);

impl ImageSlot {
    pub(crate) fn fresh() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let slot = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Keeps every table offset a valid i32 displacement.
        assert!(slot < Self::LIMIT, "image slots exhausted");
        Self(slot)
    }

    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    const LIMIT: u32 = (i32::MAX as u32) / (std::mem::size_of::<*mut u64>() as u32);

    /// Byte offset of this slot's block pointer inside a root-block table;
    /// `fresh` bounds the slot so this never overflows an i32 displacement.
    pub(crate) fn table_offset(self) -> i32 {
        (self.0 * std::mem::size_of::<*mut u64>() as u32) as i32
    }
}

/// One machine's root-block table: the block pointer of every installed
/// image by [`ImageSlot`], null where nothing is installed. Generated code
/// reads it through `VMContext::root_tables`, so the owner republishes the
/// base pointer after every call that may grow the table, and only at a
/// quiescent point (no generated frame is live during an install or a
/// retirement).
#[derive(Default)]
pub(crate) struct RootTables(Vec<*mut u64>);

impl RootTables {
    /// Publish `block` at `slot`, growing the table if needed. The returned
    /// base may differ from the previous one; the caller stores it into the
    /// machine's `VMContext` before generated code runs again.
    pub(crate) fn publish(
        &mut self,
        slot: ImageSlot,
        block: *mut u64,
    ) -> Result<*const *mut u64, ExecutionError> {
        let index = slot.index();
        if index >= self.0.len() {
            let grow = index + 1 - self.0.len();
            self.0
                .try_reserve(grow)
                .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
            self.0.resize(index + 1, std::ptr::null_mut());
        }
        self.0[index] = block;
        Ok(self.base())
    }

    pub(crate) fn clear(&mut self, slot: ImageSlot) {
        if let Some(entry) = self.0.get_mut(slot.index()) {
            *entry = std::ptr::null_mut();
        }
    }

    pub(crate) fn base(&self) -> *const *mut u64 {
        self.0.as_ptr()
    }
}

impl RootWords {
    pub(crate) fn new(length: usize) -> Result<Self, ExecutionError> {
        let mut words = Vec::new();
        words
            .try_reserve_exact(length)
            .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
        words.resize_with(length, || UnsafeCell::new(0));
        Ok(Self(words))
    }

    pub(crate) fn as_mut_ptr(&self) -> *mut u64 {
        UnsafeCell::raw_get(self.0.as_ptr())
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// The fixed address of slot `index`, as generated code and the root
    /// registries name it. `None` past the end.
    pub(crate) fn slot_address(&self, index: usize) -> Option<*mut *mut u8> {
        (index < self.0.len()).then(|| unsafe { self.as_mut_ptr().add(index) }.cast::<*mut u8>())
    }

    pub(crate) fn write(&self, index: usize, value: u64) -> Result<(), ExecutionError> {
        let word = self
            .0
            .get(index)
            .ok_or_else(|| runtime_error_without_machine(crate::host_fns::bad_pointer()))?;
        unsafe { word.get().write(value) };
        Ok(())
    }

    /// Read one slot's current value, mirroring [`Self::write`]'s exact
    /// bounds-checking. Used to resolve an import slot's published pointer
    /// during heap-top initialization, where the caller already works in
    /// [`RuntimeError`] rather than [`ExecutionError`].
    pub(crate) fn read(&self, index: usize) -> Result<u64, RuntimeError> {
        let word = self
            .0
            .get(index)
            .ok_or_else(|| crate::host_fns::bad_pointer())?;
        Ok(unsafe { *word.get() })
    }

    pub(crate) fn snapshot(&self) -> Vec<u64> {
        self.0.iter().map(|word| unsafe { *word.get() }).collect()
    }
}
