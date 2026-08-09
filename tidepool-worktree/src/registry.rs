//! The durable record of every managed worktree.
//!
//! LANE L1 owns the implementation. The types here are frozen scaffold: change
//! them only by agreement with the other lanes, since the monitor keys on
//! [`WorktreeId`] and the snapshot lane fills `snapshot_ref`.
//!
//! ## Invariants this module exists to hold
//!
//! - **Outside the source working tree.** The registry root is never inside the
//!   repository being managed. Tidepool must not dirty the tree it observes,
//!   and a registry file appearing as an untracked path would do exactly that
//!   — including turning a clean source dirty between two `create` calls.
//! - **Recorded before handed out.** A [`WorktreeReceipt`] is durable on disk
//!   before `create` returns a handle. A crash in that window may leave a
//!   registered worktree nobody asked for (recoverable, inspectable) but never
//!   a live worktree nothing recorded (invisible, unrecoverable).
//! - **Never deleted.** There is no removal API and there will not be one in
//!   v1. Deferred question 1 in the PRD owns that conversation.
//! - **Restart is a plain re-read.** Nothing about lookup may depend on
//!   in-process state that a fresh process would not have.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};

/// How a managed worktree came to exist. Recorded because "what was this seeded
/// from" is the first question asked of a tree during a post-mortem, and
/// reconstructing it from the branch graph after the fact is guesswork.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeOrigin {
    /// Seeded from the repository Tidepool itself is running against.
    CurrentRepository,
    /// Seeded from an explicit ref in the source repository.
    Ref(GitRef),
    /// Seeded from another managed worktree's current HEAD.
    Worktree(WorktreeId),
}

/// PRD 19's durable registry row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeReceipt {
    pub worktree_id: WorktreeId,
    /// Absolute path of the managed working tree. Outside the source tree.
    pub cwd: PathBuf,
    pub branch: BranchName,
    /// The commit the managed branch was rooted at.
    pub source_head: GitOid,
    /// `Some` exactly when the tree was created through
    /// [`crate::snapshot`] — the Tidepool-owned ref holding the synthetic
    /// snapshot commit. `None` for a clean creation.
    pub snapshot_ref: Option<GitRef>,
    pub origin: WorktreeOrigin,
    /// Absolute path of the source repository this tree was created from.
    pub source_repository: PathBuf,
    /// Unix epoch milliseconds.
    pub created_at_ms: i64,
}

/// The type-erased row [`WorktreeRegistry::list`] hands back. Same data as a
/// receipt plus liveness, which is a filesystem fact rather than a recorded one
/// and so must be re-derived on every listing rather than stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSummary {
    pub receipt: WorktreeReceipt,
    /// `false` when the recorded `cwd` no longer holds a git worktree — the
    /// [`WorktreeError::WorktreeLost`] condition, surfaced without erroring so
    /// a listing can show lost trees rather than failing on the first one.
    pub present: bool,
}

/// Durable, restart-surviving storage of [`WorktreeReceipt`]s.
///
/// Storage layout is L1's call, but it must be crash-safe per record (write to
/// a temporary file in the same directory, fsync, rename) — a torn registry
/// file that loses every OTHER worktree is a worse failure than the one being
/// written.
#[derive(Clone, Debug)]
pub struct WorktreeRegistry {
    root: PathBuf,
}

impl WorktreeRegistry {
    /// Open (creating if absent) a registry rooted at `root`.
    ///
    /// The caller chooses the root, and tests point it at a temp dir. There is
    /// no implicit global default here on purpose: a hardcoded `$HOME` path
    /// would make every test either share state or need an env override.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let _ = root;
        todo!("L1: create the root if absent; reject a root inside a managed source tree")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Durably record a receipt. Overwrites an existing row for the same id
    /// (the snapshot lane writes `snapshot_ref` after creation).
    pub fn put(&self, receipt: &WorktreeReceipt) -> Result<(), WorktreeError> {
        let _ = receipt;
        todo!("L1")
    }

    /// Read one row back. `Ok(None)` when the id was never registered — which
    /// is distinct from [`WorktreeError::WorktreeLost`] (registered, gone from
    /// disk), and the distinction matters: one is a typo, the other is data loss.
    pub fn get(&self, id: &WorktreeId) -> Result<Option<WorktreeReceipt>, WorktreeError> {
        let _ = id;
        todo!("L1")
    }

    /// Every registered worktree, present or lost, ordered by `created_at_ms`
    /// then id so the listing is stable across processes.
    pub fn list(&self) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        todo!("L1")
    }

    /// Mint a fresh, unused worktree id.
    pub fn mint_id(&self) -> Result<WorktreeId, WorktreeError> {
        todo!("L1: opaque and collision-free without depending on a clock alone")
    }
}
