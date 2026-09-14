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
    pub(super) fn new(machine: &'a MachineState, owner: &'a OldSpace) -> Result<Self, ExecutionError> {
        if unsafe { machine.prepared_old_space() }.is_some() {
            return Err(runtime_error(machine, RuntimeError::BadPointer));
        }
        unsafe { machine.install_prepared_old_space(owner) };
        Ok(Self { machine, _owner: owner })
    }
}

impl Drop for OldSpaceScope<'_> {
    fn drop(&mut self) {
        self.machine.clear_prepared_old_space();
    }
}

/// Fixed-address, collector-updated word slots.
pub(super) struct RootWords(Vec<UnsafeCell<u64>>);

impl RootWords {
    pub(super) fn new(length: usize) -> Result<Self, ExecutionError> {
        let mut words = Vec::new();
        words.try_reserve_exact(length).map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
        words.resize_with(length, || UnsafeCell::new(0));
        Ok(Self(words))
    }

    pub(super) fn as_mut_ptr(&self) -> *mut u64 {
        UnsafeCell::raw_get(self.0.as_ptr())
    }

    pub(super) fn write(&self, index: usize, value: u64) -> Result<(), ExecutionError> {
        let word = self.0.get(index).ok_or_else(|| runtime_error_without_machine(RuntimeError::BadPointer))?;
        unsafe { word.get().write(value) };
        Ok(())
    }

    pub(super) fn snapshot(&self) -> Vec<u64> {
        self.0.iter().map(|word| unsafe { *word.get() }).collect()
    }
}
