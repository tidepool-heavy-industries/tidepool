//! Creating and looking up managed worktrees.
//!
//! LANE L1 owns clean creation, lookup, and listing. LANE L2 owns the
//! [`WorktreeSpec::allow_dirty_snapshot`] path (see [`crate::snapshot`]).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};
use crate::label::sanitize_branch_label;
use crate::registry::{
    worktree_present, WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus, WorktreeRegistry,
    WorktreeSummary,
};
use crate::storage::now_ms;

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

    /// The source repository every `SourceCurrentRepository` worktree is
    /// created from — the path handed to [`Self::new`]. A spawner needs it
    /// to grant a worker's write sandbox the repo's `.git` (a linked
    /// worktree's git metadata lives there, not under the worktree).
    pub fn source_repository(&self) -> &Path {
        &self.source_repository
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
        let id = self.registry.mint_id()?;
        let resolved = self.resolve_source(spec, &id)?;

        fs::create_dir_all(&self.worktree_root).map_err(|e| WorktreeError::StorageFailure {
            path: self.worktree_root.clone(),
            detail: e.to_string(),
        })?;
        // Never-dirty-the-source, enforced rather than documentary: refuse a
        // worktree_root that resolves inside a git working tree (same check +
        // error as `WorktreeRegistry::open` — git walks UP from the root, so
        // the managed worktrees materialized BELOW this root never trip it).
        if let Ok(canonical_root) = self.worktree_root.canonicalize() {
            if let Ok(toplevel) = inspect::work_tree(&self.git, &canonical_root) {
                if let Ok(canonical_toplevel) = toplevel.canonicalize() {
                    if canonical_root.starts_with(&canonical_toplevel) {
                        return Err(WorktreeError::InvalidRegistryRoot {
                            root: canonical_root,
                            inside: canonical_toplevel,
                        });
                    }
                }
            }
        }
        let cwd = self.worktree_root.join(id.as_str());
        let branch = BranchName::from_raw(format!(
            "{TIDEPOOL_BRANCH_PREFIX}/{}-{}",
            sanitize_branch_label(&spec.label),
            id.as_str()
        ));

        let provisional = WorktreeReceipt {
            worktree_id: id.clone(),
            cwd: cwd.clone(),
            branch: branch.clone(),
            source_head: resolved.seed.clone(),
            snapshot_ref: resolved.snapshot_ref.clone(),
            origin: resolved.origin.clone(),
            source_repository: resolved.git_repository.clone(),
            created_at_ms: now_ms(),
            status: WorktreeRecordStatus::Provisional,
        };
        self.registry.put(&provisional)?;

        let args: Vec<OsString> = vec![
            "worktree".into(),
            "add".into(),
            "-q".into(),
            "-b".into(),
            OsString::from(branch.as_str()),
            cwd.clone().into_os_string(),
            OsString::from(resolved.seed.as_str()),
        ];
        self.git.try_run(&resolved.git_repository, &args)?;

        let finalized = WorktreeReceipt {
            status: WorktreeRecordStatus::Finalized,
            ..provisional
        };
        self.registry.put(&finalized)?;

        Ok(WorktreeHandle::from_receipt(finalized))
    }

    /// Resolve what commit a new worktree should be rooted at, and where.
    fn resolve_source(
        &self,
        spec: &WorktreeSpec,
        id: &WorktreeId,
    ) -> Result<ResolvedSeed, WorktreeError> {
        match &spec.source {
            WorktreeSource::CurrentRepository => {
                let (seed, snapshot_ref) =
                    self.resolve_dirty_or_clean(&self.source_repository, spec.dirty_policy, id)?;
                Ok(ResolvedSeed {
                    seed,
                    snapshot_ref,
                    origin: WorktreeOrigin::CurrentRepository,
                    git_repository: self.source_repository.clone(),
                })
            }
            WorktreeSource::Ref(r) => {
                // A named ref is already-committed content: there is nothing
                // uncommitted to check, so no dirty/in-progress gate applies.
                let out = self
                    .git
                    .try_run(&self.source_repository, &["rev-parse", r.as_str()])?;
                Ok(ResolvedSeed {
                    seed: GitOid::from_raw(out.trimmed()),
                    snapshot_ref: None,
                    origin: WorktreeOrigin::Ref(r.clone()),
                    git_repository: self.source_repository.clone(),
                })
            }
            WorktreeSource::Worktree(wid) => {
                let handle = self
                    .lookup(wid)?
                    .ok_or_else(|| WorktreeError::WorktreeNotRegistered(wid.clone()))?;
                let cwd = handle.cwd().to_path_buf();
                let (seed, snapshot_ref) =
                    self.resolve_dirty_or_clean(&cwd, spec.dirty_policy, id)?;
                Ok(ResolvedSeed {
                    seed,
                    snapshot_ref,
                    origin: WorktreeOrigin::Worktree(wid.clone()),
                    git_repository: cwd,
                })
            }
        }
    }

    /// Check `repo` clean/in-progress and produce the commit to root a new
    /// branch at. An in-progress merge/rebase/cherry-pick refuses regardless
    /// of `policy` — a synthetic commit of a half-merged tree is a
    /// reproducible base for the wrong program, dirty-snapshot opt-in or not.
    fn resolve_dirty_or_clean(
        &self,
        repo: &Path,
        policy: DirtyPolicy,
        id: &WorktreeId,
    ) -> Result<(GitOid, Option<GitRef>), WorktreeError> {
        if let Some(kind) = inspect::in_progress(&self.git, repo)? {
            return Err(WorktreeError::SourceOperationInProgress(kind));
        }
        let summary = inspect::dirty_summary(&self.git, repo)?;
        if summary.is_clean() {
            let out = self.git.try_run(repo, &["rev-parse", "HEAD"])?;
            return Ok((GitOid::from_raw(out.trimmed()), None));
        }
        match policy {
            DirtyPolicy::RequireClean => Err(WorktreeError::SourceDirty(summary)),
            DirtyPolicy::AllowDirtySnapshot => {
                let temp_index_dir = self
                    .worktree_root
                    .join(".tidepool-snapshot-index")
                    .join(id.as_str());
                let receipt =
                    crate::snapshot::snapshot_source(&self.git, repo, id, &temp_index_dir)?;
                Ok((receipt.snapshot_commit, Some(receipt.snapshot_ref)))
            }
        }
    }

    /// Look a worktree up by durable id. `Err(WorktreeLost)` when it is
    /// registered but gone from disk; `Ok(None)` when it was never registered.
    pub fn lookup(&self, id: &WorktreeId) -> Result<Option<WorktreeHandle>, WorktreeError> {
        match self.registry.get(id)? {
            None => Ok(None),
            Some(receipt) => {
                if worktree_present(&receipt.cwd) {
                    Ok(Some(WorktreeHandle::from_receipt(receipt)))
                } else {
                    Err(WorktreeError::WorktreeLost(id.clone()))
                }
            }
        }
    }

    pub fn list(&self) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        self.registry.list()
    }

    /// Fresh read of `handle`'s CURRENT git `HEAD`, performed at call time.
    ///
    /// Deliberately NOT [`WorktreeHandle::source_head`] (the seed commit a
    /// managed branch was rooted at, recorded once at `create` and frozen
    /// forever after) and NOT anything [`crate::monitor::WorktreeMonitor`]
    /// last reconciled — the verb exists precisely so a resident spanning
    /// loop iterations can see HEAD movement the monitor never observed, closing the
    /// gap between one loop iteration's handlers unregistering and the next
    /// loop iteration's re-registering. A cached or stale answer here silently
    /// reopens that exact gap.
    ///
    /// `git rev-parse HEAD` resolves to the current commit whether the tree
    /// is on a normal branch checkout or detached, so no special-casing is
    /// needed for detached HEAD.
    pub fn worktree_head(&self, handle: &WorktreeHandle) -> Result<GitOid, WorktreeError> {
        if !worktree_present(handle.cwd()) {
            return Err(WorktreeError::WorktreeLost(handle.id().clone()));
        }
        let out = self.git.try_run(handle.cwd(), &["rev-parse", "HEAD"])?;
        Ok(GitOid::from_raw(out.trimmed()))
    }
}

/// What a new worktree should be rooted at, and in which repository the
/// `git worktree add` invocation must run.
struct ResolvedSeed {
    seed: GitOid,
    snapshot_ref: Option<GitRef>,
    origin: WorktreeOrigin,
    git_repository: PathBuf,
}
