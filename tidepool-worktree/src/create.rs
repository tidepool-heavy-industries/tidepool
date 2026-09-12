//! Creating and looking up retained managed linked worktrees, including
//! dirty-source snapshots.

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
use tidepool_repr::ActorPath;

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
    /// Where managed linked working trees are materialized. Outside the source tree.
    worktree_root: PathBuf,
    /// The repository `from_current_repository` means.
    source_repository: PathBuf,
}

/// Independent child Git state awaiting its inherited working-file view.
/// Its durable receipt remains provisional; it is not a launchable worktree.
#[derive(Debug)]
pub struct PreparedSourceWorktree {
    receipt: WorktreeReceipt,
}

impl PreparedSourceWorktree {
    pub fn receipt(&self) -> &WorktreeReceipt {
        &self.receipt
    }

    /// This pointer belongs in the child's private source upper directory.
    pub fn git_file(&self) -> PathBuf {
        self.receipt.cwd.join(".git")
    }
}

impl WorktreeManager {
    /// Preserve Cargo freshness after a committed fallback materializes a new
    /// checkout. Only exact tracked regular-file matches get donor mtimes.
    /// Missing, filtered, or changing files retain their fresh checkout times.
    #[cfg(target_os = "linux")]
    pub fn restore_matching_mtimes_from_view(
        &self,
        handle: &WorktreeHandle,
        donor: &tidepool_node::MountNamespace,
        visible_root: &Path,
    ) -> Result<usize, WorktreeError> {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;

        fn stable(before: &fs::Metadata, after: &fs::Metadata) -> bool {
            use std::os::unix::fs::MetadataExt;
            (
                before.dev(),
                before.ino(),
                before.len(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec(),
            ) == (
                after.dev(),
                after.ino(),
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec(),
            )
        }

        fn identical(mut donor: &fs::File, mut target: &fs::File) -> std::io::Result<bool> {
            let mut source_bytes = [0u8; 65_536];
            let mut target_bytes = [0u8; 65_536];
            loop {
                let count = donor.read(&mut source_bytes)?;
                if count == 0 {
                    return Ok(target.read(&mut target_bytes[..1])? == 0);
                }
                target.read_exact(&mut target_bytes[..count])?;
                if source_bytes[..count] != target_bytes[..count] {
                    return Ok(false);
                }
            }
        }

        let tracked = self
            .git
            .try_run(handle.cwd(), &["ls-files", "--cached", "-z"])?;
        let checkout =
            fs::File::open(handle.cwd()).map_err(|error| WorktreeError::StorageFailure {
                path: handle.cwd().to_owned(),
                detail: error.to_string(),
            })?;
        let mut restored = 0;
        for name in tracked.nul_fields() {
            let relative = Path::new(name);
            if !relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                continue;
            }
            let Ok(source) = donor.open_view_file(&visible_root.join(relative)) else {
                continue;
            };
            let Ok(target) = rustix::fs::openat2(
                &checkout,
                relative,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
                rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_SYMLINKS,
            ) else {
                continue;
            };
            let target = fs::File::from(target);
            let (Ok(before), Ok(destination)) = (source.metadata(), target.metadata()) else {
                continue;
            };
            if !before.is_file()
                || !destination.is_file()
                || before.len() != destination.len()
                || before.mode() & 0o111 != destination.mode() & 0o111
                || !identical(&source, &target).unwrap_or(false)
            {
                continue;
            }
            let Ok(after) = source.metadata() else {
                continue;
            };
            if !stable(&before, &after) {
                continue;
            }
            if let Ok(modified) = before.modified() {
                if target
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .is_ok()
                {
                    restored += 1;
                }
            }
        }
        Ok(restored)
    }

    /// Preserve working files before the lifecycle owner retires their mounts.
    /// The caller must have stopped writers and closed hosted-work admission.
    /// Index, HEAD and objects already live in the shared Git administrative tree.
    #[cfg(target_os = "linux")]
    pub fn materialize_retired_view(
        &self,
        id: &WorktreeId,
        namespace: &tidepool_node::MountNamespace,
        visible: &Path,
    ) -> Result<(), WorktreeError> {
        let failure = |error: std::io::Error| WorktreeError::StorageFailure {
            path: visible.to_owned(),
            detail: error.to_string(),
        };
        let _capture = self.git.try_capture().ok_or_else(|| {
            failure(std::io::Error::other(
                "Git operation active during retirement",
            ))
        })?;
        let mut receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        let view = match self.registry.views.resolve(&receipt.cwd).map_err(failure)? {
            Some(view) => view,
            None if receipt.status == WorktreeRecordStatus::Finalized => return Ok(()),
            None => {
                return Err(failure(std::io::Error::other(
                    "worktree has no installed view",
                )))
            }
        };
        if !view.namespace.same_view_as(namespace).map_err(failure)? || view.root != visible {
            return Err(failure(std::io::Error::other("retirement view mismatch")));
        }
        let stage = tempfile::tempdir_in(
            receipt
                .cwd
                .parent()
                .ok_or_else(|| failure(std::io::Error::other("worktree lacks parent")))?,
        )
        .map_err(failure)?;
        let mut producer = namespace
            .host_command(visible, "tar".as_ref())
            .map_err(failure)?
            .args([
                "--acls",
                "--xattrs",
                "--sparse",
                "--exclude=./.git",
                "--exclude=./.shoal",
                "-cf",
                "-",
                ".",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(failure)?;
        let input = producer
            .stdout
            .take()
            .ok_or_else(|| failure(std::io::Error::other("missing archive pipe")))?;
        let consumer = std::process::Command::new("tar")
            .args(["--acls", "--xattrs", "--sparse", "-xf", "-", "-C"])
            .arg(stage.path())
            .stdin(input)
            .status();
        if consumer.is_err() {
            let _ = producer.kill();
        }
        let produced = producer.wait().map_err(failure)?;
        if !consumer.map_err(failure)?.success() || !produced.success() {
            return Err(failure(std::io::Error::other(
                "working-file preservation failed",
            )));
        }
        // Flush the independent copy before dropping any mount-backed source.
        let mut pending = vec![stage.path().to_owned()];
        let mut directories = Vec::new();
        while let Some(path) = pending.pop() {
            let metadata = std::fs::symlink_metadata(&path).map_err(failure)?;
            if metadata.is_dir() {
                directories.push(path.clone());
                for entry in std::fs::read_dir(path).map_err(failure)? {
                    pending.push(entry.map_err(failure)?.path());
                }
            } else if metadata.is_file() {
                std::fs::File::open(path)
                    .and_then(|file| file.sync_all())
                    .map_err(failure)?;
            }
        }
        for directory in directories.into_iter().rev() {
            std::fs::File::open(directory)
                .and_then(|file| file.sync_all())
                .map_err(failure)?;
        }
        // Original mounts remain authoritative until all files and the registry
        // transition succeed. Failure retains them for a retry.
        for entry in std::fs::read_dir(&receipt.cwd).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            if entry.file_name() == ".git" || entry.file_name() == ".shoal" {
                continue;
            }
            if entry.file_type().map_err(failure)?.is_dir() {
                std::fs::remove_dir_all(entry.path()).map_err(failure)?;
            } else {
                std::fs::remove_file(entry.path()).map_err(failure)?;
            }
        }
        for entry in std::fs::read_dir(stage.path()).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            std::fs::rename(entry.path(), receipt.cwd.join(entry.file_name())).map_err(failure)?;
        }
        std::fs::File::open(&receipt.cwd)
            .and_then(|file| file.sync_all())
            .map_err(failure)?;
        receipt.status = WorktreeRecordStatus::Finalized;
        self.registry.put(&receipt)?;
        self.registry
            .views
            .remove(&receipt.cwd, namespace)
            .map_err(failure)?;
        Ok(())
    }

    pub fn new(
        git: GitCli,
        registry: WorktreeRegistry,
        worktree_root: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
    ) -> Self {
        #[cfg(target_os = "linux")]
        let git = git.with_worktree_views(registry.views.clone());
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

    /// Root containing every managed linked working tree owned by this manager.
    pub fn managed_root(&self) -> &Path {
        &self.worktree_root
    }

    /// Repository used by [`WorktreeSource::CurrentRepository`].
    pub fn source_repository(&self) -> &Path {
        &self.source_repository
    }

    /// Register the clean source checkout as a typed integration target.
    ///
    /// This records an existing checkout; it does not create, reset, stage, or
    /// otherwise mutate Git state. Repeated calls return the same durable
    /// receipt. The dirty and in-progress checks happen on every call so a
    /// stale handle cannot turn the conservative merge path into an implicit
    /// overwrite of user work.
    pub fn register_source_checkout(&self) -> Result<WorktreeHandle, WorktreeError> {
        let source = inspect::work_tree(&self.git, &self.source_repository)?;
        if let Some(kind) = inspect::in_progress(&self.git, &source)? {
            return Err(WorktreeError::SourceOperationInProgress(kind));
        }
        let dirty = inspect::dirty_summary(&self.git, &source)?;
        if !dirty.is_clean() {
            return Err(WorktreeError::SourceDirty(dirty));
        }

        let canonical_source =
            source
                .canonicalize()
                .map_err(|error| WorktreeError::StorageFailure {
                    path: source.clone(),
                    detail: error.to_string(),
                })?;
        if let Some(receipt) = self
            .registry
            .list_with_git(&self.git)?
            .into_iter()
            .find_map(|summary| {
                (summary.receipt.origin == WorktreeOrigin::SourceCheckout
                    && summary.receipt.cwd.canonicalize().ok().as_ref() == Some(&canonical_source))
                .then_some(summary.receipt)
            })
        {
            return Ok(WorktreeHandle::from_receipt(receipt));
        }

        let branch = self.git.try_run(
            &canonical_source,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        let head = self
            .git
            .try_run(&canonical_source, &["rev-parse", "HEAD"])?;
        let receipt = WorktreeReceipt {
            worktree_id: self.registry.mint_id()?,
            cwd: canonical_source.clone(),
            branch: BranchName::from_raw(branch.trimmed()),
            source_head: GitOid::from_raw(head.trimmed()),
            snapshot_ref: None,
            origin: WorktreeOrigin::SourceCheckout,
            source_repository: canonical_source,
            created_at_ms: now_ms(),
            status: WorktreeRecordStatus::Finalized,
        };
        self.registry.put(&receipt)?;
        Ok(WorktreeHandle::from_receipt(receipt))
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
        self.create_with_branch(spec, None)
    }

    /// Create a worktree whose readable Git branch is the exact projection of
    /// an already allocated actor lineage. The actor registry is the naming
    /// authority; this owner performs the Git mutation and durable receipt.
    pub fn create_for_actor_path(
        &self,
        spec: &WorktreeSpec,
        actor_path: &ActorPath,
    ) -> Result<WorktreeHandle, WorktreeError> {
        self.create_with_branch(spec, Some(BranchName::from_raw(actor_path.git_branch())))
    }

    /// Allocate a committed fallback without rejecting or committing dirty files.
    /// Resolve the selected checkout's current HEAD once, then use that exact oid.
    pub fn create_committed_fork(
        &self,
        source: &WorktreeSource,
        actor_path: &ActorPath,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let (repository, origin) = match source {
            WorktreeSource::CurrentRepository => (
                self.source_repository.clone(),
                WorktreeOrigin::CurrentRepository,
            ),
            WorktreeSource::Worktree(id) => (
                self.lookup(id)?
                    .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?
                    .cwd()
                    .to_owned(),
                WorktreeOrigin::Worktree(id.clone()),
            ),
            WorktreeSource::Ref(reference) => (
                self.source_repository.clone(),
                WorktreeOrigin::Ref(reference.clone()),
            ),
        };
        let revision = match source {
            WorktreeSource::Ref(reference) => reference.as_str(),
            _ => "HEAD",
        };
        let seed = GitOid::from_raw(
            self.git
                .try_run(
                    &repository,
                    &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
                )?
                .trimmed(),
        );
        self.materialize(
            self.registry.mint_id()?,
            ResolvedSeed {
                seed,
                snapshot_ref: None,
                origin,
                git_repository: repository.clone(),
                source_repository: repository,
            },
            &actor_path.to_string(),
            Some(BranchName::from_raw(actor_path.git_branch())),
            None,
        )
    }

    /// Prepare a child of the selected live checkout without checking out
    /// files or converting staging into a commit. The source-view owner invokes
    /// this while holding native mutation admission and retains the prepared
    /// checkout until its source mount is installed. Explicit commit seeds use
    /// `create_for_actor_path` instead.
    pub fn prepare_inherited_source(
        &self,
        source: &WorktreeSource,
        actor_path: &ActorPath,
    ) -> Result<PreparedSourceWorktree, WorktreeError> {
        let (source, origin) = match source {
            WorktreeSource::CurrentRepository => (
                self.source_repository.clone(),
                WorktreeOrigin::CurrentRepository,
            ),
            WorktreeSource::Worktree(id) => {
                let handle = self
                    .lookup(id)?
                    .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
                (
                    handle.cwd().to_path_buf(),
                    WorktreeOrigin::Worktree(id.clone()),
                )
            }
            WorktreeSource::Ref(_) => {
                return Err(WorktreeError::WorktreeAuthorityDenied(
                    "an explicit Git ref has no live checkout to inherit".into(),
                ));
            }
        };
        let source = &source;
        if let Some(kind) = inspect::in_progress(&self.git, source)? {
            return Err(WorktreeError::SourceOperationInProgress(kind));
        }
        self.prepare_worktree_root()?;
        let temporary = tempfile::Builder::new()
            .prefix(".source-index-")
            .tempdir_in(&self.worktree_root)
            .map_err(|error| crate::storage::storage_failure(&self.worktree_root, error))?;
        let index = temporary.path().join("index");
        let source_index = self.git.try_run(
            source,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?;
        let index_utf8 = camino::Utf8Path::from_path(&index).ok_or_else(|| {
            crate::storage::storage_failure(&index, "index path is not valid UTF-8")
        })?;
        let source_directory = inspect::git_dir(&self.git, source)?;
        let source_directory_utf8 =
            camino::Utf8Path::from_path(&source_directory).ok_or_else(|| {
                crate::storage::storage_failure(
                    &source_directory,
                    "Git directory is not valid UTF-8",
                )
            })?;
        let temporary_git = self
            .git
            .on_host()
            .with_env("GIT_DIR", source_directory_utf8.as_str())
            .with_env("GIT_INDEX_FILE", index_utf8.as_str());
        if self.git.try_exists(Path::new(source_index.trimmed()))? {
            self.git
                .copy_file_to_host(Path::new(source_index.trimmed()), &index)?;
            // Resolve split-index dependencies while the original Git directory
            // still supplies them. Only the temporary index may be rewritten.
            temporary_git.try_run(&self.worktree_root, &["update-index", "--no-split-index"])?;
        } else {
            // A missing index is an empty staging area, not an index at HEAD.
            temporary_git.try_run(&self.worktree_root, &["read-tree", "--empty"])?;
        }
        let seed = GitOid::from_raw(self.git.try_run(source, &["rev-parse", "HEAD"])?.trimmed());
        let resolved = ResolvedSeed {
            seed,
            snapshot_ref: None,
            origin,
            git_repository: source.clone(),
            source_repository: source.clone(),
        };
        let handle = self.materialize(
            self.registry.mint_id()?,
            resolved,
            &actor_path.to_string(),
            Some(BranchName::from_raw(actor_path.git_branch())),
            Some(&index),
        )?;
        Ok(PreparedSourceWorktree {
            receipt: handle.receipt,
        })
    }

    /// Settle an unexposed source preparation as an ordinary committed checkout.
    /// Only this provisional receipt authorizes replacing its private index.
    pub fn finish_committed_source(
        &self,
        prepared: PreparedSourceWorktree,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let mut receipt = prepared.receipt;
        if receipt.status != WorktreeRecordStatus::Provisional
            || self.registry.get(&receipt.worktree_id)?.as_ref() != Some(&receipt)
        {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "committed fallback requires its original provisional checkout".into(),
            ));
        }
        receipt.source_head = GitOid::from_raw(
            self.git
                .try_run(
                    &receipt.source_repository,
                    &["rev-parse", "--verify", "HEAD^{commit}"],
                )?
                .trimmed(),
        );
        self.git.on_host().try_run(
            &receipt.cwd,
            &["reset", "--hard", receipt.source_head.as_str()],
        )?;
        receipt.status = WorktreeRecordStatus::Finalized;
        self.registry.put(&receipt)?;
        Ok(WorktreeHandle::from_receipt(receipt))
    }

    /// Complete Git preparation only after the child's actual mounted view is
    /// accessible. All manager clones and their Git clients then resolve the
    /// registered checkout path through that same retained filesystem view.
    #[cfg(target_os = "linux")]
    pub fn finish_inherited_source(
        &self,
        prepared: PreparedSourceWorktree,
        namespace: tidepool_node::MountNamespace,
        visible_root: &Path,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let receipt = prepared.receipt;
        if self.registry.get(&receipt.worktree_id)?.as_ref() != Some(&receipt) {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "source preparation does not match this worktree registry".into(),
            ));
        }
        let finalized = self
            .registry
            .install_view(&receipt, namespace, visible_root)?;
        Ok(WorktreeHandle::from_receipt(finalized))
    }

    /// Bind a completed checkout to its retained launch view, or reattach that
    /// view after recovery. Verify the registered Git identity without resetting
    /// HEAD, the index, or working files. Native execution and mount construction
    /// remain with their resource owners.
    #[cfg(target_os = "linux")]
    pub fn mount_worktree(
        &self,
        id: &WorktreeId,
        namespace: tidepool_node::MountNamespace,
        visible_root: &Path,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status == WorktreeRecordStatus::Provisional {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "filesystem binding requires a completed checkout".into(),
            ));
        }
        let mounted = self
            .registry
            .install_view(&receipt, namespace, visible_root)?;
        Ok(WorktreeHandle::from_receipt(mounted))
    }

    /// Replace exactly the preparation view before native execution is released.
    #[cfg(target_os = "linux")]
    pub fn activate_worktree(
        &self,
        id: &WorktreeId,
        expected: &tidepool_node::MountNamespace,
        namespace: tidepool_node::MountNamespace,
        visible_root: &Path,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status == WorktreeRecordStatus::Provisional {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "workspace activation requires completed preparation".into(),
            ));
        }
        Ok(WorktreeHandle::from_receipt(self.registry.activate_view(
            &receipt,
            expected,
            namespace,
            visible_root,
        )?))
    }

    fn create_with_branch(
        &self,
        spec: &WorktreeSpec,
        named_branch: Option<BranchName>,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let id = self.registry.mint_id()?;
        let resolved = self.resolve_source(spec, &id)?;

        self.materialize(id, resolved, &spec.label, named_branch, None)
    }

    fn materialize(
        &self,
        id: WorktreeId,
        resolved: ResolvedSeed,
        label: &str,
        named_branch: Option<BranchName>,
        inherited_index: Option<&Path>,
    ) -> Result<WorktreeHandle, WorktreeError> {
        self.prepare_worktree_root()?;
        let cwd = self.worktree_root.join(id.as_str());
        let branch = named_branch.unwrap_or_else(|| {
            BranchName::from_raw(format!(
                "{TIDEPOOL_BRANCH_PREFIX}/{}-{}",
                sanitize_branch_label(label),
                id.as_str()
            ))
        });

        let provisional = WorktreeReceipt {
            worktree_id: id.clone(),
            cwd: cwd.clone(),
            branch: branch.clone(),
            source_head: resolved.seed.clone(),
            snapshot_ref: resolved.snapshot_ref.clone(),
            origin: resolved.origin.clone(),
            source_repository: resolved.source_repository.clone(),
            created_at_ms: now_ms(),
            status: WorktreeRecordStatus::Provisional,
        };
        self.registry.put(&provisional)?;

        // Native linked worktrees give each actor separate working files,
        // index, and HEAD while keeping commits and branches in one ordinary
        // repository namespace shared with the root.
        let mut args: Vec<OsString> = vec!["worktree".into(), "add".into(), "-q".into()];
        if inherited_index.is_some() {
            args.push("--no-checkout".into());
        }
        args.extend([
            "-b".into(),
            OsString::from(branch.as_str()),
            cwd.clone().into_os_string(),
            OsString::from(resolved.seed.as_str()),
        ]);
        let common = inspect::git_common_dir(&self.git, &resolved.git_repository)?;
        let host_git = self.git.on_host();
        host_git.try_run(&common, &args)?;

        if let Some(index) = inherited_index {
            let directory = inspect::git_dir(&host_git, &cwd)?;
            fs::copy(index, directory.join("index"))
                .map_err(|error| crate::storage::storage_failure(&directory, error))?;
            // Working files are not installed yet. Keep the durable receipt
            // provisional until the source-view owner completes that step.
            return Ok(WorktreeHandle::from_receipt(provisional));
        }

        let finalized = WorktreeReceipt {
            status: WorktreeRecordStatus::Finalized,
            ..provisional
        };
        self.registry.put(&finalized)?;

        Ok(WorktreeHandle::from_receipt(finalized))
    }

    fn prepare_worktree_root(&self) -> Result<(), WorktreeError> {
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
        Ok(())
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
                    source_repository: self.source_repository.clone(),
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
                    source_repository: self.source_repository.clone(),
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
                    git_repository: cwd.clone(),
                    // Record the immediate seed checkout even though linked
                    // worktrees share its common Git namespace.
                    source_repository: cwd,
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

    /// Look a finalized worktree up by durable id. Missing registered storage
    /// returns `WorktreeLost`; present provisional storage cannot grant a
    /// usable handle. `Ok(None)` means it was never registered.
    pub fn lookup(&self, id: &WorktreeId) -> Result<Option<WorktreeHandle>, WorktreeError> {
        match self.registry.get(id)? {
            None => Ok(None),
            Some(receipt) => {
                if worktree_present(&self.git, &receipt.cwd)? {
                    if receipt.status == WorktreeRecordStatus::Provisional {
                        return Err(WorktreeError::WorktreeAuthorityDenied(format!(
                            "worktree {id} initialization is not finalized"
                        )));
                    }
                    Ok(Some(WorktreeHandle::from_receipt(receipt)))
                } else {
                    Err(WorktreeError::WorktreeLost(id.clone()))
                }
            }
        }
    }

    pub fn list(&self) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        self.registry.list_with_git(&self.git)
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
        if !worktree_present(&self.git, handle.cwd())? {
            return Err(WorktreeError::WorktreeLost(handle.id().clone()));
        }
        let out = self.git.try_run(handle.cwd(), &["rev-parse", "HEAD"])?;
        Ok(GitOid::from_raw(out.trimmed()))
    }

    /// Observe the current submitted repository state as one bounded,
    /// internally consistent operation. This does not seal the worktree.
    pub fn observe_submission(
        &self,
        handle: &WorktreeHandle,
    ) -> Result<crate::SubmissionObservation, WorktreeError> {
        crate::submission::observe(&self.git, handle)
    }
}

/// What a new managed linked worktree should be rooted at.
struct ResolvedSeed {
    seed: GitOid,
    snapshot_ref: Option<GitRef>,
    origin: WorktreeOrigin,
    /// Repository from which `git worktree add` is invoked.
    git_repository: PathBuf,
    /// Durable record of the immediate seed checkout.
    source_repository: PathBuf,
}
