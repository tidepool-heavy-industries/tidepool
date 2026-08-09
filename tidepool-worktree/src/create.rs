//! Creating and looking up managed worktrees.
//!
//! LANE L1 owns clean creation, lookup, and listing. LANE L2 owns the
//! [`WorktreeSpec::allow_dirty_snapshot`] path (see [`crate::snapshot`]).
//!
//! ## The vocabulary is closed
//!
//! Creation, lookup, inspection, events. There is no `rebase`, `merge`,
//! `cherry_pick`, conflict resolution, or branch promotion here, and adding one
//! is out of scope for this PRD rather than merely unimplemented — the git work
//! belongs to coding agents with their native tools, and Tidepool observes what
//! the repository became.

use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::git::GitCli;
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};
use crate::registry::{WorktreeReceipt, WorktreeRegistry, WorktreeSummary};

/// Tidepool's owned branch namespace. Every managed branch lives under this
/// prefix so a managed branch can never collide with, or be mistaken for, a
/// branch the operator made.
pub const TIDEPOOL_BRANCH_PREFIX: &str = "tidepool/worktree";

/// Tidepool's owned ref namespace for synthetic snapshot commits. Deliberately
/// NOT under `refs/heads/`: a snapshot is a reproducible base, not a branch the
/// operator is invited to check out, and keeping it out of the branch namespace
/// keeps it out of every `git branch` listing the operator reads.
pub const TIDEPOOL_SNAPSHOT_REF_PREFIX: &str = "refs/tidepool/snapshots";

/// What to seed a managed worktree from, and under what dirty-source policy.
///
/// Built with [`WorktreeSpec::from_current_repository`] /
/// [`WorktreeSpec::from_ref`] / [`WorktreeSpec::from_worktree`] and refined
/// with [`WorktreeSpec::allow_dirty_snapshot`], mirroring the authored Haskell
/// vocabulary one-for-one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSpec {
    pub source: WorktreeSource,
    /// A caller-supplied label (`"dev-tree/root"`). Sanitized into the managed
    /// branch name; never used as a path or an identity.
    pub label: String,
    pub dirty_policy: DirtyPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeSource {
    CurrentRepository,
    Ref(GitRef),
    Worktree(WorktreeId),
}

/// Clean-by-default is the whole safety property; the opt-in is explicit at
/// every call site because it is spelled at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirtyPolicy {
    RequireClean,
    AllowDirtySnapshot,
}

impl WorktreeSpec {
    pub fn from_current_repository(label: impl Into<String>) -> Self {
        Self {
            source: WorktreeSource::CurrentRepository,
            label: label.into(),
            dirty_policy: DirtyPolicy::RequireClean,
        }
    }

    pub fn from_ref(git_ref: GitRef, label: impl Into<String>) -> Self {
        Self {
            source: WorktreeSource::Ref(git_ref),
            label: label.into(),
            dirty_policy: DirtyPolicy::RequireClean,
        }
    }

    pub fn from_worktree(id: WorktreeId, label: impl Into<String>) -> Self {
        Self {
            source: WorktreeSource::Worktree(id),
            label: label.into(),
            dirty_policy: DirtyPolicy::RequireClean,
        }
    }

    #[must_use]
    pub fn allow_dirty_snapshot(mut self) -> Self {
        self.dirty_policy = DirtyPolicy::AllowDirtySnapshot;
        self
    }
}

/// A live managed worktree. Cheap to clone; it is a name plus its recorded
/// facts, not an open handle to anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeHandle {
    receipt: WorktreeReceipt,
}

impl WorktreeHandle {
    pub fn from_receipt(receipt: WorktreeReceipt) -> Self {
        Self { receipt }
    }

    pub fn id(&self) -> &WorktreeId {
        &self.receipt.worktree_id
    }

    pub fn cwd(&self) -> &Path {
        &self.receipt.cwd
    }

    pub fn branch(&self) -> &BranchName {
        &self.receipt.branch
    }

    pub fn source_head(&self) -> &GitOid {
        &self.receipt.source_head
    }

    pub fn receipt(&self) -> &WorktreeReceipt {
        &self.receipt
    }
}

/// Creates, looks up, and lists managed worktrees against one source repository
/// and one registry.
#[derive(Clone, Debug)]
pub struct WorktreeManager {
    git: GitCli,
    registry: WorktreeRegistry,
    /// Where managed working trees are materialized. Outside the source tree.
    worktree_root: PathBuf,
    /// The repository `from_current_repository` means.
    source_repository: PathBuf,
}

impl WorktreeManager {
    pub fn new(
        git: GitCli,
        registry: WorktreeRegistry,
        worktree_root: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
    ) -> Self {
        Self {
            git,
            registry,
            worktree_root: worktree_root.into(),
            source_repository: source_repository.into(),
        }
    }

    pub fn git(&self) -> &GitCli {
        &self.git
    }

    pub fn registry(&self) -> &WorktreeRegistry {
        &self.registry
    }

    /// Create a managed worktree.
    ///
    /// Ordering that L1 must hold, and why: resolve the seed commit, mint the
    /// id, materialize the worktree, THEN record the receipt — except that the
    /// receipt write must not be the last thing that can fail, or a crash
    /// leaves an unrecorded live worktree. Record a provisional row before
    /// materializing and finalize it after; a provisional row that never
    /// finalized is discoverable as such.
    pub fn create(&self, spec: &WorktreeSpec) -> Result<WorktreeHandle, WorktreeError> {
        let _ = spec;
        todo!("L1 (clean) / L2 (dirty snapshot path)")
    }

    /// Look a worktree up by durable id. `Err(WorktreeLost)` when it is
    /// registered but gone from disk; `Ok(None)` when it was never registered.
    pub fn lookup(&self, id: &WorktreeId) -> Result<Option<WorktreeHandle>, WorktreeError> {
        let _ = id;
        todo!("L1: must work in a FRESH process against only on-disk state")
    }

    pub fn list(&self) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        todo!("L1")
    }
}
