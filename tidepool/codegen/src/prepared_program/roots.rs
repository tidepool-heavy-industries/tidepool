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
