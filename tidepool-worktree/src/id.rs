//! Opaque identities. Every one of these is a newtype rather than a bare
//! `String`/`u64` so a git OID can never be passed where a worktree id is
//! wanted, and so `EventId` (runtime identity) stays visibly distinct from
//! `GitOid` (domain data) — PRD 19 states that split explicitly.

use serde::{Deserialize, Serialize};

/// Stable, durable identity of a managed worktree. Survives restart; minted
/// once at creation and recorded in the registry before the worktree is handed
/// out. Opaque to authored code: it is not a path, a branch, or an OID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorktreeId(String);

impl WorktreeId {
    /// Wrap an already-minted identity (registry load, test fixture).
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorktreeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runtime identity of one observed repository event.
///
/// LOAD-BEARING: a single underlying change (a normal commit) yields a `commit`
/// observation and a `headChanged` observation that SHARE one `EventId`. That
/// sharing is how a consumer can tell "these are two views of one thing" from
/// "these are two things", so the id is minted once per reconciliation pass,
/// not once per emitted observation.
///
/// The multi-commit case follows from that and is worth stating so it is not
/// re-litigated: when a pass coalesces several commits into one `Advanced`, the
/// pass emits one `headChanged` and one `commit` per gained commit, and ALL of
/// them carry the pass's single id. So the id means "these facts were reconciled
/// together", which is exactly what a consumer can act on — it does NOT mean
/// "these describe one commit", and nothing should read it that way. Each gained
/// commit is independently and honestly inferable, so dropping all but one would
/// hide real review targets from a `commit` subscriber for no gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(pub u64);

/// Runtime identity of one live subscription (`withHandler`'s registration).
/// Never reused within a process; a subscription that unregisters spends its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubscriptionId(pub u64);

/// A git object id, as git spelled it (full 40-hex unless a caller explicitly
/// asked for an abbreviation). Domain data, not runtime identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GitOid(String);

impl GitOid {
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GitOid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Any reference a caller may seed a worktree from: a branch, tag, remote ref,
/// or raw OID. Resolved by git, not parsed here.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GitRef(String);

impl GitRef {
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A branch name inside Tidepool's owned namespace (see
/// [`crate::create::TIDEPOOL_BRANCH_PREFIX`]) or, for `from_ref`, whatever the
/// caller named. Stored without the `refs/heads/` prefix.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BranchName(String);

impl BranchName {
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BranchName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
