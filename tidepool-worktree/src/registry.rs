//! The durable record of every managed worktree.
//!
//! The types here are durable wire/storage vocabulary: change
//! them only by agreement with the other lanes.
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
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};
use crate::storage::{storage_failure, DurableJsonDir};

/// Directory under the registry root holding one JSON file per worktree id.
const RECORDS_DIR: &str = "records";

/// Whether the recorded `cwd` still holds a real git working tree. A plain
/// `Path::exists` would be fooled by a directory left behind with its `.git`
/// file removed; this reconciles like everything else in this crate.
pub(crate) fn worktree_present(cwd: &Path) -> bool {
    cwd.exists() && inspect::work_tree(&GitCli::new(), cwd).is_ok()
}

/// How a managed worktree came to exist. Recorded because "what was this seeded
/// from" is the first question asked of a tree during a post-mortem, and
/// reconstructing it from the branch graph after the fact is guesswork.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeOrigin {
    /// The source checkout itself, registered as a conservative integration
    /// target. Unlike the other variants, no linked worktree was created.
    SourceCheckout,
    /// Seeded from the repository Tidepool itself is running against.
    CurrentRepository,
    /// Seeded from an explicit ref in the source repository.
    Ref(GitRef),
    /// Seeded from another managed worktree's current HEAD.
    Worktree(WorktreeId),
}

/// Whether a registry row was recorded before or after the worktree it
/// describes was actually materialized.
///
/// `create` writes `Provisional` before `git worktree add` runs and
/// `Finalized` after. A crash between those two writes leaves a
/// `Provisional` row with no matching worktree on disk — discoverable via
/// `list`, distinguishable from a `Finalized` row that a human later removed
/// by hand (both report `present: false`, but only the latter was ever live).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeRecordStatus {
    Provisional,
    Finalized,
}

/// The durable registry row for one managed worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeReceipt {
    pub worktree_id: WorktreeId,
    /// Absolute path of the managed working tree. Outside the source tree.
    pub cwd: PathBuf,
    pub branch: BranchName,
    /// The commit the managed branch was rooted at. For a snapshot creation
    /// this is the synthetic snapshot commit, not the pre-snapshot source
    /// `HEAD` (which `snapshot_ref` lets you reach via its parent).
    pub source_head: GitOid,
    /// `Some` exactly when the tree was created through
    /// [`crate::snapshot`] — the Tidepool-owned ref holding the synthetic
    /// snapshot commit. `None` for a clean creation.
    pub snapshot_ref: Option<GitRef>,
    pub origin: WorktreeOrigin,
    /// Absolute path of the immediate checkout this tree was created from.
    /// Linked worktrees share a common Git namespace, but retaining this path
    /// preserves provenance. A from-worktree child records its parent's cwd.
    pub source_repository: PathBuf,
    /// Unix epoch milliseconds.
    pub created_at_ms: i64,
    /// Provisional until `create` finishes materializing the worktree. See
    /// [`WorktreeRecordStatus`].
    pub status: WorktreeRecordStatus,
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
/// Crash-safe per record (write to a temporary file in the same directory,
/// fsync, rename) — a torn registry file that loses every OTHER worktree is
/// a worse failure than the one being written.
#[derive(Clone, Debug)]
pub struct WorktreeRegistry {
    root: PathBuf,
    records: DurableJsonDir,
}

impl WorktreeRegistry {
    /// Open (creating if absent) a registry rooted at `root`.
    ///
    /// The caller chooses the root, and tests point it at a temp dir. There is
    /// no implicit global default here on purpose: a hardcoded `$HOME` path
    /// would make every test either share state or need an env override.
    ///
    /// Refuses a root that resolves inside ANY git working tree with
    /// [`WorktreeError::InvalidRegistryRoot`]. `open` takes only `root`, not a
    /// specific source repository, so the check cannot be "is this the tree we
    /// were told not to dirty" — it is necessarily the broader "is this inside
    /// a working tree at all", which is a strictly safer reading of the
    /// never-dirty-the-source invariant.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| storage_failure(&root, e))?;
        let canonical_root = root.canonicalize().map_err(|e| storage_failure(&root, e))?;

        let git = GitCli::new();
        if let Ok(toplevel) = inspect::work_tree(&git, &canonical_root) {
            if let Ok(canonical_toplevel) = toplevel.canonicalize() {
                if canonical_root.starts_with(&canonical_toplevel) {
                    return Err(WorktreeError::InvalidRegistryRoot {
                        root: canonical_root,
                        inside: canonical_toplevel,
                    });
                }
            }
        }

        let records = DurableJsonDir::open(canonical_root.join(RECORDS_DIR))?;

        Ok(Self {
            root: canonical_root,
            records,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Durably record a receipt. Overwrites an existing row for the same id
    /// (the snapshot lane writes `snapshot_ref` after creation).
    pub fn put(&self, receipt: &WorktreeReceipt) -> Result<(), WorktreeError> {
        #[allow(clippy::expect_used, reason = "serialize WorktreeReceipt")]
        let bytes = serde_json::to_vec_pretty(receipt).expect("serialize WorktreeReceipt");
        self.records.write(receipt.worktree_id.as_str(), &bytes)
    }

    /// Read one row back. `Ok(None)` when the id was never registered — which
    /// is distinct from [`WorktreeError::WorktreeLost`] (registered, gone from
    /// disk), and the distinction matters: one is a typo, the other is data loss.
    ///
    /// A record that fails to deserialize is a [`WorktreeError::StorageFailure`],
    /// not `Ok(None)`: records are written via [`DurableJsonDir::write`], so a
    /// crash mid-write cannot land a torn file at this path — a corrupt record here
    /// means something else went wrong (bit rot, a hand edit, a filesystem
    /// fault), and collapsing that into "never registered" would hide it
    /// behind the exact typo/data-loss distinction this function's contract
    /// is careful to keep apart.
    pub fn get(&self, id: &WorktreeId) -> Result<Option<WorktreeReceipt>, WorktreeError> {
        let Some(bytes) = self.records.read(id.as_str())? else {
            return Ok(None);
        };
        let receipt = serde_json::from_slice(&bytes)
            .map_err(|e| storage_failure(&self.records.path_for(id.as_str()), e))?;
        Ok(Some(receipt))
    }

    /// Every registered worktree, present or lost, ordered by `created_at_ms`
    /// then id so the listing is stable across processes.
    ///
    /// A LOST worktree (registered, gone from disk) is reported as
    /// `present: false` rather than failing the listing — one missing tree must
    /// never hide the others. A CORRUPT record is different and does halt the
    /// listing with [`WorktreeError::StorageFailure`] naming that file.
    ///
    /// The asymmetry is deliberate and worth knowing before it surprises
    /// someone: loss is an expected outcome of a human removing a directory,
    /// while corruption is not an expected byproduct of anything this crate
    /// does (records are written to a temp file in the same directory, fsynced,
    /// then renamed, so a torn record cannot land here). The tension with
    /// retain-first is real — one bad file makes every other worktree
    /// temporarily unlistable — but the error names the exact path to fix and
    /// no worktree, branch, or record is lost, so the state is recoverable by
    /// inspection rather than by guesswork. If corruption ever turns out to be
    /// routine rather than exceptional, the fix is to make a listing able to
    /// REPRESENT an unreadable row, not to skip it silently; skipping would
    /// make a retained worktree quietly disappear, which is the exact failure
    /// retain-first exists to prevent. See `L6-storage-errors-receipt.md`.
    pub fn list(&self) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        let mut receipts = Vec::new();
        for (path, bytes) in self.records.read_all()? {
            let receipt: WorktreeReceipt =
                serde_json::from_slice(&bytes).map_err(|e| storage_failure(&path, e))?;
            receipts.push(receipt);
        }
        receipts.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.worktree_id.cmp(&b.worktree_id))
        });
        Ok(receipts
            .into_iter()
            .map(|receipt| {
                let present = worktree_present(&receipt.cwd);
                WorktreeSummary { receipt, present }
            })
            .collect())
    }

    /// Mint a fresh, unused worktree id: `wt-<uuid v4>`. A v4 UUID's 122 bits
    /// of randomness make a collision vanishingly unlikely on its own; the
    /// existence check below is defense-in-depth, not the uniqueness
    /// mechanism itself.
    pub fn mint_id(&self) -> Result<WorktreeId, WorktreeError> {
        loop {
            let candidate = WorktreeId::from_raw(format!("wt-{}", uuid::Uuid::new_v4()));
            if !self.records.exists(candidate.as_str()) {
                return Ok(candidate);
            }
        }
    }
}
