//! [`SnapshotDigest`] — the blake3 identity type the durable log's
//! `Event::SnapshotFrozen`/`Event::BranchInvocation` variants carry.
//!
//! The context-snapshot/branch feature this type used to back (freezing a
//! node's transcript as a named cache root, forking a child off it) was
//! deleted (sol cross-family review findings 7/8: dead vestige, no
//! production caller). The type itself survives only because those two log
//! event variants are WIRE FORMAT — a durable log written before the
//! deletion may still carry them, and the variants stay in place
//! (unemitted) rather than break replay of an old log. Nothing in this tree
//! mints a [`SnapshotDigest`] anymore.

use serde::{Deserialize, Serialize};

/// The blake3 identity of a frozen context prefix, hex-encoded. A newtype for
/// the same reason `tidepool_runtime::cache::InvocationKey` is one: a raw
/// string must not be mistaken for a computed digest at an intern/lookup
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotDigest(pub String);

impl SnapshotDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
