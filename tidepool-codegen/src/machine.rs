//! Shared execution errors and machine statistics.

pub use crate::machine_state::{ExternalStorageStats, MachineDisposition};
pub use crate::resource_ledger::{CancelHandle, ResourceCounts};
/// A read-only snapshot of one machine's heap and code counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    pub nursery_bytes: usize,
    pub live_bytes: usize,
    pub gc_count: u64,
    pub fragments: u64,
}
