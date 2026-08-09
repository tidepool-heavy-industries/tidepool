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
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::WorktreeError;
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};

/// Directory under the registry root holding one JSON file per worktree id.
const RECORDS_DIR: &str = "records";

/// Current time as Unix epoch milliseconds. Shared with [`crate::create`] so
/// `created_at_ms` and binding timestamps come from one clock read helper.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as i64
}

/// Write `bytes` to `path` crash-safely: a temp file in the SAME directory,
/// fsynced, then renamed over the target. A torn write cannot land at `path`
/// — either the old content is still there or the new content is, never a
/// partial file — and a sibling record in the same directory is never
/// touched by writing this one.
fn write_atomic(path: &Path, bytes: &[u8]) {
    let dir = path.parent().expect("record path has a parent directory");
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .unwrap_or_else(|e| panic!("create temp file in {}: {e}", dir.display()));
    tmp.write_all(bytes)
        .unwrap_or_else(|e| panic!("write temp file for {}: {e}", path.display()));
    tmp.as_file()
        .sync_all()
        .unwrap_or_else(|e| panic!("fsync temp file for {}: {e}", path.display()));
    tmp.persist(path)
        .unwrap_or_else(|e| panic!("rename temp file into {}: {e}", path.display()));
    // Best-effort directory fsync so the rename itself survives a crash; not
    // fatal if the platform does not support fsync on a directory handle.
    if let Ok(dirf) = fs::File::open(dir) {
        let _ = dirf.sync_all();
    }
}

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

/// PRD 19's durable registry row.
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
    /// Absolute path of the source repository this tree was created from.
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
    ///
    /// Refuses a root that resolves inside ANY git working tree with
    /// [`WorktreeError::InvalidRegistryRoot`]. `open` takes only `root`, not a
    /// specific source repository, so the check cannot be "is this the tree we
    /// were told not to dirty" — it is necessarily the broader "is this inside
    /// a working tree at all", which is a strictly safer reading of the
    /// never-dirty-the-source invariant.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)
            .unwrap_or_else(|e| panic!("create registry root {}: {e}", root.display()));
        let canonical_root = root
            .canonicalize()
            .unwrap_or_else(|e| panic!("canonicalize registry root {}: {e}", root.display()));

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

        fs::create_dir_all(canonical_root.join(RECORDS_DIR)).unwrap_or_else(|e| {
            panic!("create records dir under {}: {e}", canonical_root.display())
        });

        Ok(Self {
            root: canonical_root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn record_path(&self, id: &WorktreeId) -> PathBuf {
        self.root
            .join(RECORDS_DIR)
            .join(format!("{}.json", id.as_str()))
    }

    /// Durably record a receipt. Overwrites an existing row for the same id
    /// (the snapshot lane writes `snapshot_ref` after creation).
    pub fn put(&self, receipt: &WorktreeReceipt) -> Result<(), WorktreeError> {
        let bytes = serde_json::to_vec_pretty(receipt).expect("serialize WorktreeReceipt");
        write_atomic(&self.record_path(&receipt.worktree_id), &bytes);
        Ok(())
    }

    /// Read one row back. `Ok(None)` when the id was never registered — which
    /// is distinct from [`WorktreeError::WorktreeLost`] (registered, gone from
    /// disk), and the distinction matters: one is a typo, the other is data loss.
    pub fn get(&self, id: &WorktreeId) -> Result<Option<WorktreeReceipt>, WorktreeError> {
        let path = self.record_path(id);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!("corrupt registry record {}: {e}", path.display())
            }))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => panic!("read registry record {}: {e}", path.display()),
        }
    }

    /// Every registered worktree, present or lost, ordered by `created_at_ms`
    /// then id so the listing is stable across processes.
    pub fn list(&self) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        let dir = self.root.join(RECORDS_DIR);
        let mut receipts = Vec::new();
        for entry in
            fs::read_dir(&dir).unwrap_or_else(|e| panic!("read records dir {}: {e}", dir.display()))
        {
            let entry = entry.expect("read records dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)
                .unwrap_or_else(|e| panic!("read registry record {}: {e}", path.display()));
            let receipt: WorktreeReceipt = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("corrupt registry record {}: {e}", path.display()));
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

    /// Mint a fresh, unused worktree id. Combines wall-clock time, process
    /// id, an in-process counter, and process-local randomness so two
    /// creates in the same millisecond — an ordinary event, not a hazard —
    /// cannot alias, without depending on the clock alone.
    pub fn mint_id(&self) -> Result<WorktreeId, WorktreeError> {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};

        static NEXT: AtomicU64 = AtomicU64::new(0);

        loop {
            let now = now_ms();
            let pid = std::process::id();
            let counter = NEXT.fetch_add(1, Ordering::Relaxed);
            let random = RandomState::new().build_hasher().finish();
            let candidate =
                WorktreeId::from_raw(format!("wt-{now:x}-{pid:x}-{counter:x}-{random:016x}"));
            if !self.record_path(&candidate).exists() {
                return Ok(candidate);
            }
        }
    }
}
