//! Shared plumbing for this crate's durable-store modules — [`registry`],
//! [`binding`](crate::binding), and [`journal`](crate::journal) — plus their
//! callers outside this crate (`tidepool-agent`'s spawn saga,
//! `tidepool-handlers`' repository-event handler). Both `now_ms` and
//! `storage_failure` used to be copied independently at each call site; this
//! module is the one place either is defined now.
//!
//! [`registry`]: crate::registry
//!
//! ## The `now_ms` behavior this consolidation chose
//!
//! Four independent copies existed before this consolidation: `tidepool-agent`'s
//! spawn saga, this crate's `registry.rs`, this crate's `journal.rs`, and
//! `tidepool-handlers`' repository-event handler (the `Tick` `firedAtMs`
//! stamp). Three of the four already agreed on PANICKING on a pre-epoch
//! clock; the fourth — the event handler's Tick stamp, documented there as
//! "an observability stamp only" — silently degraded to `0` instead. That
//! divergence is a bug surfaced by consolidation, not one this module
//! preserves: every caller now panics. A pre-epoch wall clock is a broken
//! machine no caller can act on, and a Tick silently stamped `0` on a broken
//! clock is a worse failure — a wrong-looking-right observability record —
//! than losing that one tick loudly.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::WorktreeError;

/// Current time as Unix epoch milliseconds.
///
/// # Panics
///
/// If the system clock reads before the Unix epoch — see the module docs for
/// why every caller here deliberately panics rather than degrading.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64
}

/// Build a [`WorktreeError::StorageFailure`] naming the path that actually
/// failed, from any underlying error with a `Display` impl (`std::io::Error`
/// for I/O, `serde_json::Error` for a corrupt record).
pub fn storage_failure(path: &Path, detail: impl std::fmt::Display) -> WorktreeError {
    WorktreeError::StorageFailure {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}
