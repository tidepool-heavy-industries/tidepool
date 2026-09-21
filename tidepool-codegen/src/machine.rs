//! Shared execution errors and machine statistics.

pub use crate::machine_state::{ExternalStorageStats, MachineDisposition};
pub use crate::resource_ledger::{CancelHandle, ResourceCounts};
use tidepool_effect::EffectError;

/// Error type for failures at the native execution boundary.
#[derive(Debug, thiserror::Error)]
pub enum JitError {
    #[error("effect dispatch error: {0}")]
    Effect(#[from] EffectError),
    #[error("invalid suspension state: {0}")]
    InvalidSuspensionState(&'static str),
}

/// A read-only snapshot of one machine's heap and code counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    pub nursery_bytes: usize,
    pub live_bytes: usize,
    pub gc_count: u64,
    pub fragments: u64,
}
