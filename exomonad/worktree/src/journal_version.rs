//! The current worktree observation-batch journal format.

/// This build accepts and writes only complete observation batches. Earlier
/// event-per-row journals cannot establish atomic reconciliation boundaries.
pub const CURRENT: u32 = 2;
