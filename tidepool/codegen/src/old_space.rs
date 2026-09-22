//! Stable roots and descriptor arenas retained by the prepared-STG machine.
//!
//! Prepared values move during collection, so every binding and handle keeps
//! the address of a persistently registered pointer cell. The collector
//! rewrites that cell in place and consumers always load through [`RootSlot`].

mod prepared;

pub(crate) use prepared::PreparedCompactionStats;

/// The stable, GC-updated slot holding a retained value's live heap pointer.
///
/// The slot address remains valid until the prepared machine drops. Collection
/// may rewrite the pointer stored in the slot, so consumers must call
/// [`Self::current`] instead of caching its value.
#[derive(Copy, Clone, Debug)]
pub struct RootSlot(*mut *mut u8);

impl RootSlot {
    /// Wrap a persistently registered pointer-cell address.
    ///
    /// # Safety
    /// `slot` must be non-null, valid, and registered as a persistent root for
    /// at least as long as this value can be reached.
    pub unsafe fn new(slot: *mut *mut u8) -> Self {
        Self(slot)
    }

    /// Load the collector-current heap pointer.
    ///
    /// # Safety
    /// The slot must still satisfy the invariant on [`Self::new`].
    pub unsafe fn current(self) -> *mut u8 {
        *self.0
    }

    /// Return the stable pointer-cell address.
    pub fn addr(self) -> *mut *mut u8 {
        self.0
    }
}

/// Prepared descriptor arenas and stable root cells owned by one invocation.
pub struct OldSpace {
    /// Heap cells whose addresses are registered as persistent roots.
    /// Boxing keeps each address stable if this vector reallocates.
    #[allow(clippy::vec_box)]
    pub(super) slots: Vec<Box<*mut u8>>,
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
            slots: Vec::new(),
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
