//! Stable roots and descriptor arenas retained by the prepared-STG machine.
//!
//! Prepared values move during collection. Ledger and frame entries own
//! stable registered cells; callers borrow current values through [`RootRef`].

mod prepared;

pub(crate) use prepared::PreparedCompactionStats;

/// Internal physical view of a registered root cell. Its owning ledger or
/// frame must remain alive throughout every use.
#[derive(Copy, Clone, Debug)]
pub(crate) struct RootSlot(*mut *mut u8);

impl RootSlot {
    /// # Safety
    /// The cell owner must remain alive throughout the physical read.
    pub(crate) unsafe fn current(self) -> *mut u8 {
        unsafe { self.0.read() }
    }

    pub(crate) fn addr(self) -> *mut *mut u8 {
        self.0
    }
}

/// A readable root view borrowed from its machine's live cell owner.
/// Mutable machine operations cannot run while this view is subsequently used.
/// Raw addresses are pointer receipts, not recoverable safe root views.
#[derive(Debug)]
pub struct RootRef<'machine> {
    cell: &'machine OwnedRootCell,
}

impl RootRef<'_> {
    pub fn addr(&self) -> *mut *mut u8 {
        self.cell.addr()
    }

    /// Load the current heap pointer without retaining or authenticating the
    /// pointed object. Dereferencing it still requires the heap's contract.
    pub fn current(&self) -> *mut u8 {
        // The borrowed owner guarantees this cell remains allocated.
        unsafe { self.cell.current() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootRegistration {
    Persistent,
    Stowed,
}

/// The sole owner of one stable cell and its collector registration.
/// Boxes never move; UnsafeCell permits collector updates through the registered
/// address while immutable views of this owner exist. This owner never escapes
/// the machine's ledger/frame or a non-collecting issuance/transfer span.
#[derive(Debug)]
pub(crate) struct OwnedRootCell {
    cell: Box<std::cell::UnsafeCell<*mut u8>>,
    machine: std::rc::Weak<crate::machine_state::MachineState>,
    registration: RootRegistration,
}

impl OwnedRootCell {
    pub(crate) fn new(
        machine: &std::rc::Rc<crate::machine_state::MachineState>,
        pointer: *mut u8,
    ) -> Result<Self, crate::host_fns::RuntimeError> {
        machine.try_reserve_persistent_roots(1)?;
        let cell = Self {
            cell: Box::new(std::cell::UnsafeCell::new(pointer)),
            machine: std::rc::Rc::downgrade(machine),
            registration: RootRegistration::Persistent,
        };
        machine.register_persistent_root(cell.addr());
        machine.root_cell_created();
        Ok(cell)
    }

    pub(crate) fn addr(&self) -> *mut *mut u8 {
        self.cell.get()
    }

    pub(crate) unsafe fn current(&self) -> *mut u8 {
        // The backing allocation remains alive for this owner borrow.
        unsafe { self.addr().read() }
    }

    pub(crate) fn borrowed(&self) -> RootRef<'_> {
        RootRef { cell: self }
    }

    pub(crate) fn physical(&self) -> RootSlot {
        // Internal readers borrow the owning ledger or frame for their walk.
        RootSlot(self.addr())
    }

    pub(crate) fn stow(&mut self) -> Result<(), crate::host_fns::RuntimeError> {
        self.transition(RootRegistration::Stowed)
    }

    pub(crate) fn make_persistent(&mut self) -> Result<(), crate::host_fns::RuntimeError> {
        self.transition(RootRegistration::Persistent)
    }

    fn transition(
        &mut self,
        destination: RootRegistration,
    ) -> Result<(), crate::host_fns::RuntimeError> {
        if destination == self.registration {
            return Ok(());
        }
        let machine = self
            .machine
            .upgrade()
            .expect("a live machine owns its cells");
        // Admit every fallible destination allocation before detaching the
        // prior registration. No native code, collection or callback runs here.
        match destination {
            RootRegistration::Persistent => machine.try_reserve_persistent_roots(1)?,
            RootRegistration::Stowed => machine.try_reserve_stowed_roots(1)?,
        }
        match self.registration {
            RootRegistration::Persistent => machine.deregister_persistent_root(self.addr()),
            RootRegistration::Stowed => machine.deregister_stowed_root(self.addr()),
        }
        match destination {
            RootRegistration::Persistent => machine.register_persistent_root(self.addr()),
            RootRegistration::Stowed => machine.register_stowed_root(self.addr()),
        }
        self.registration = destination;
        Ok(())
    }
}

impl Drop for OwnedRootCell {
    fn drop(&mut self) {
        if let Some(machine) = self.machine.upgrade() {
            match self.registration {
                RootRegistration::Persistent => machine.deregister_persistent_root(self.addr()),
                RootRegistration::Stowed => machine.deregister_stowed_root(self.addr()),
            }
            machine.root_cell_destroyed();
        }
        // The Box is freed after its registration has been settled.
    }
}

/// Prepared descriptor arenas owned by one invocation.
pub struct OldSpace {
    /// Descriptor arenas retained by invocation-local promotion.
    pub(crate) prepared_arenas: Vec<tidepool_heap::descriptor_region::DescriptorArena>,
}

// SAFETY: the owner moves only while quiescent; its raw pointers are accessed
// exclusively by the session's prepared-machine thread.
unsafe impl Send for OldSpace {}

impl Default for OldSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl OldSpace {
    pub fn new() -> Self {
        Self {
            prepared_arenas: Vec::new(),
        }
    }

    /// Bytes retained in prepared descriptor arenas.
    pub fn prepared_bytes_used(&self) -> usize {
        self.prepared_arenas
            .iter()
            .map(tidepool_heap::descriptor_region::DescriptorArena::bytes_used)
            .sum()
    }
}

#[cfg(test)]
mod root_cell_tests {
    use super::*;
    use crate::{host_fns::RuntimeError, machine_state::MachineState};
    use std::rc::Rc;

    #[test]
    fn registered_cell_transfers_preserve_storage_and_refusal_preserves_prior_class() {
        let machine = Rc::new(MachineState::new());
        let mut cell = OwnedRootCell::new(&machine, std::ptr::null_mut()).unwrap();
        let address = cell.borrowed().addr() as usize;
        assert_eq!(machine.root_cell_allocation_counts(), (1, 1));
        machine.fail_next_stowed_root_reservation.set(true);
        assert!(matches!(cell.stow(), Err(RuntimeError::HeapOverflow)));
        assert_eq!(
            (
                machine.persistent_roots_count(),
                machine.stowed_roots_count()
            ),
            (1, 0)
        );
        assert_eq!(cell.borrowed().addr() as usize, address);
        cell.stow().unwrap();
        machine.fail_next_persistent_root_reservation.set(true);
        assert!(matches!(
            cell.make_persistent(),
            Err(RuntimeError::HeapOverflow)
        ));
        assert_eq!(
            (
                machine.persistent_roots_count(),
                machine.stowed_roots_count()
            ),
            (0, 1)
        );
        cell.make_persistent().unwrap();
        assert_eq!(cell.borrowed().addr() as usize, address);
        assert_eq!(machine.root_cell_allocation_counts(), (1, 1));
        drop(cell);
        assert_eq!(machine.root_cell_allocation_counts(), (0, 1));
        assert_eq!(
            (
                machine.persistent_roots_count(),
                machine.stowed_roots_count()
            ),
            (0, 0)
        );
    }

    #[test]
    fn registered_cells_release_during_unwind_and_failed_issuance_allocates_nothing() {
        let machine = Rc::new(MachineState::new());
        machine.fail_next_persistent_root_reservation.set(true);
        assert!(matches!(
            OwnedRootCell::new(&machine, std::ptr::null_mut()),
            Err(RuntimeError::HeapOverflow)
        ));
        assert_eq!(machine.root_cell_allocation_counts(), (0, 0));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _persistent = OwnedRootCell::new(&machine, std::ptr::null_mut()).unwrap();
            let mut stowed = OwnedRootCell::new(&machine, std::ptr::null_mut()).unwrap();
            stowed.stow().unwrap();
            panic!("exercise owner unwind");
        }));
        assert!(outcome.is_err());
        assert_eq!(machine.root_cell_allocation_counts(), (0, 2));
        assert_eq!(
            (
                machine.persistent_roots_count(),
                machine.stowed_roots_count()
            ),
            (0, 0)
        );
    }
}
