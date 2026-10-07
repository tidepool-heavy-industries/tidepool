//! Creating and looking up retained managed linked worktrees, including
//! dirty-source snapshots.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};
use crate::registry::{
    validate_external_root, worktree_present, WorktreeOrigin, WorktreeReceipt,
    WorktreeRecordStatus, WorktreeRegistry, WorktreeSummary,
};
use crate::storage::now_ms;

/// Tidepool's owned branch namespace. Every managed branch lives under this
/// prefix so a managed branch can never collide with, or be mistaken for, a
/// branch the operator made.
pub const EXOMONAD_BRANCH_PREFIX: &str = "exomonad/worktree";

/// Tidepool's owned ref namespace for synthetic snapshot commits. Deliberately
/// NOT under `refs/heads/`: a snapshot is a reproducible base, not a branch the
/// operator is invited to check out, and keeping it out of the branch namespace
/// keeps it out of every `git branch` listing the operator reads.
pub const EXOMONAD_SNAPSHOT_REF_PREFIX: &str = "refs/exomonad/snapshots";

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

/// How long retirement waits for running host Git commands.
#[cfg(target_os = "linux")]
const RETIREMENT_GIT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(target_os = "linux")]
fn restoration_budget(layers: &[PathBuf]) -> std::io::Result<u64> {
    let mut bytes = 0_u64;
    for layer in layers {
        for entry in walkdir::WalkDir::new(layer)
            .follow_links(false)
            .follow_root_links(false)
        {
            let entry = entry.map_err(std::io::Error::other)?;
            let metadata = entry.metadata()?;
            bytes = bytes.saturating_add(4096);
            if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn sync_restored_tree(root: &Path) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .follow_root_links(false)
        .contents_first(true)
    {
        let entry = entry.map_err(std::io::Error::other)?;
        let path = entry.path();
        if entry.file_type().is_dir() || entry.file_type().is_file() {
            fs::File::open(path)?.sync_all()?;
        }
    }
    Ok(())
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

#[cfg(all(test, target_os = "linux"))]
mod restoration_walk_tests {
    use super::*;

    #[test]
    fn restoration_budget_counts_entries_and_does_not_follow_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let layer = directory.path().join("layer");
        let nested = layer.join("nested");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("file"), b"12345").unwrap();
        fs::write(&outside, vec![0; 1 << 20]).unwrap();
        std::os::unix::fs::symlink(&outside, layer.join("link")).unwrap();

        assert_eq!(restoration_budget(&[layer]).unwrap(), 4 * 4096 + 5);
    }

    #[test]
    fn restoration_walk_does_not_follow_a_symlink_root() {
        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), b"outside").unwrap();
        let layer = directory.path().join("layer");
        std::os::unix::fs::symlink(&outside, &layer).unwrap();

        assert_eq!(restoration_budget(&[layer.clone()]).unwrap(), 4096);
        sync_restored_tree(&layer).unwrap();
    }

    #[test]
    fn restoration_sync_propagates_a_walk_error() {
        let directory = tempfile::tempdir().unwrap();
        assert!(sync_restored_tree(&directory.path().join("missing")).is_err());
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

    pub fn branch(&self) -> Option<&BranchName> {
        self.receipt.branch.as_ref()
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
        donor: &exomonad_node::MountNamespace,
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

    /// Publish durable custody of the sealed source layers before detaching the
    /// live mount. The caller has stopped writers, settled publication, synced
    /// the upper, and pinned every layer's resource against reclamation.
    #[cfg(target_os = "linux")]
    pub fn retain_retired_view(
        &self,
        id: &WorktreeId,
        namespace: &exomonad_node::MountNamespace,
        visible: &Path,
        layers: Vec<PathBuf>,
    ) -> Result<(), WorktreeError> {
        let failure = |error: std::io::Error| WorktreeError::StorageFailure {
            path: visible.to_owned(),
            detail: error.to_string(),
        };
        // Retirement waits out ordinary host Git traffic (a parent inspecting
        // this branch); only a Git operation that outlasts the wait retains
        // the workspace.
        let _capture = self
            .git
            .capture_within(&self.source_repository, RETIREMENT_GIT_WAIT)?
            .ok_or_else(|| {
                failure(std::io::Error::other(
                    "Git operation still active after waiting for retirement",
                ))
            })?;
        let mut receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status == WorktreeRecordStatus::Retained {
            let manifest = self.registry.retained_manifest(&receipt)?.ok_or_else(|| {
                failure(std::io::Error::other("retained view manifest is missing"))
            })?;
            if manifest.layers == layers {
                return Ok(());
            }
            return Err(failure(std::io::Error::other(
                "retained view layers differ from published custody",
            )));
        }
        if receipt.status != WorktreeRecordStatus::Mounted {
            return Err(failure(std::io::Error::other(
                "worktree has no mounted source view",
            )));
        }
        let view = match self.registry.views.resolve(&receipt.cwd).map_err(failure)? {
            Some(view) => view,
            None => {
                return Err(failure(std::io::Error::other(
                    "worktree has no installed view",
                )))
            }
        };
        if !view.namespace.same_view_as(namespace).map_err(failure)? || view.root != visible {
            return Err(failure(std::io::Error::other("retirement view mismatch")));
        }
        let descriptor = self.registry.retained_manifest(&receipt)?.ok_or_else(|| {
            failure(std::io::Error::other(
                "mounted source descriptor is missing",
            ))
        })?;
        if descriptor.pending_layers.is_some() || descriptor.layers != layers {
            return Err(failure(std::io::Error::other(
                "retirement source differs from the confirmed mounted descriptor",
            )));
        }
        receipt.status = WorktreeRecordStatus::Retained;
        self.registry.finish_retention(&receipt)?;
        Ok(())
    }

    /// Record the exact live source view while its publication gate excludes
    /// writers. This descriptor is the recovery authority after host loss.
    #[cfg(target_os = "linux")]
    pub fn record_mounted_layers(
        &self,
        id: &WorktreeId,
        layers: Vec<PathBuf>,
    ) -> Result<(), WorktreeError> {
        let receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        self.registry.put_mounted_layers(&receipt, layers)
    }

    /// Pin both sides of a source rotation before its mount can change.
    #[cfg(target_os = "linux")]
    pub fn record_mounted_transition(
        &self,
        id: &WorktreeId,
        before: Vec<PathBuf>,
        after: Vec<PathBuf>,
    ) -> Result<(), WorktreeError> {
        let receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        self.registry
            .put_mounted_transition(&receipt, before, after)
    }

    /// Called only after the facade proves the predecessor process retired.
    /// An ambiguous rotation cannot be resolved from surviving filesystem
    /// paths, so it remains visibly Mounted and keeps both layer sets pinned.
    #[cfg(target_os = "linux")]
    pub fn seal_orphaned_mounted_view(&self, id: &WorktreeId) -> Result<(), WorktreeError> {
        let mut receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status != WorktreeRecordStatus::Mounted {
            return Ok(());
        }
        let manifest = self.registry.retained_manifest(&receipt)?.ok_or_else(|| {
            crate::storage::storage_failure(&receipt.cwd, "mounted source descriptor is missing")
        })?;
        if manifest.pending_layers.is_some() {
            return Err(crate::storage::storage_failure(
                &receipt.cwd,
                "mounted source rotation is ambiguous",
            ));
        }
        let upper = manifest.layers.last().ok_or_else(|| {
            crate::storage::storage_failure(&receipt.cwd, "mounted source descriptor is empty")
        })?;
        sync_restored_tree(upper).map_err(|error| crate::storage::storage_failure(upper, error))?;
        receipt.status = WorktreeRecordStatus::Retained;
        self.registry.finish_retention(&receipt)
    }

    /// A committed-source fallback already has ordinary host working files.
    #[cfg(target_os = "linux")]
    pub fn release_host_view(
        &self,
        id: &WorktreeId,
        namespace: &exomonad_node::MountNamespace,
        visible: &Path,
    ) -> Result<(), WorktreeError> {
        let receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status != WorktreeRecordStatus::Finalized {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "host view release requires a finalized checkout".into(),
            ));
        }
        let view = self
            .registry
            .views
            .resolve(&receipt.cwd)
            .map_err(|error| crate::storage::storage_failure(&receipt.cwd, error))?;
        let Some(view) = view else {
            return Ok(());
        };
        if !view
            .namespace
            .same_view_as(namespace)
            .map_err(|error| crate::storage::storage_failure(&receipt.cwd, error))?
            || view.root != visible
        {
            return Err(crate::storage::storage_failure(
                &receipt.cwd,
                "host view mismatch",
            ));
        }
        self.registry
            .views
            .remove(&receipt.cwd, namespace)
            .map_err(|error| crate::storage::storage_failure(&receipt.cwd, error))
    }

    /// Reconstruct ordinary host files only for a consumer that needs a usable
    /// checkout path. The manifest remains authoritative until the complete
    /// copy and directory entries have been flushed.
    #[cfg(target_os = "linux")]
    pub fn restore_retained_view(&self, id: &WorktreeId) -> Result<(), WorktreeError> {
        use exomonad_node::copy_admission::{
            CopyAdmission, DEFAULT_MAX_COPY_BYTES, DEFAULT_MIN_FREE_BYTES,
        };
        use exomonad_node::{ProcessMountBoundary, BUBBLEWRAP_PROGRAM};

        let receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status == WorktreeRecordStatus::Finalized {
            return Ok(());
        }
        if receipt.status != WorktreeRecordStatus::Retained {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "worktree does not have a restorable retained view".into(),
            ));
        }
        let manifest = self.registry.retained_manifest(&receipt)?.ok_or_else(|| {
            crate::storage::storage_failure(&receipt.cwd, "retained view manifest is missing")
        })?;
        let failure = |error: std::io::Error| crate::storage::storage_failure(&receipt.cwd, error);
        let planned = restoration_budget(&manifest.layers).map_err(failure)?;
        let _admission = CopyAdmission::acquire(
            &receipt.cwd,
            planned,
            DEFAULT_MAX_COPY_BYTES,
            DEFAULT_MIN_FREE_BYTES,
            RETIREMENT_GIT_WAIT,
        )
        .map_err(failure)?;
        let _capture = self
            .git
            .capture_within(&self.source_repository, RETIREMENT_GIT_WAIT)?
            .ok_or_else(|| {
                failure(std::io::Error::other(
                    "Git operation still active before restoration",
                ))
            })?;
        // Another process can finish restoration while this caller waits for
        // admission. Its finalized receipt is the only authority to return.
        let mut receipt = self
            .registry
            .get(id)?
            .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
        if receipt.status == WorktreeRecordStatus::Finalized {
            return Ok(());
        }
        if receipt.status != WorktreeRecordStatus::Retained {
            return Err(failure(std::io::Error::other(
                "retained view changed during admission",
            )));
        }
        let parent = receipt
            .cwd
            .parent()
            .ok_or_else(|| failure(std::io::Error::other("worktree lacks parent")))?;
        let stage = tempfile::tempdir_in(parent).map_err(failure)?;
        let view = stage.path().join("view");
        let upper = stage.path().join("upper");
        let work = stage.path().join("work");
        let copy = stage.path().join("copy");
        for path in [&view, &upper, &work, &copy] {
            fs::create_dir(path).map_err(failure)?;
        }
        let boundary = ProcessMountBoundary::new(&view, [view.clone()], [view.clone()])
            .map_err(|error| failure(std::io::Error::other(error)))?
            .with_overlay_view(manifest.layers.clone(), &upper, &work, &view)
            .map_err(|error| failure(std::io::Error::other(error)))?
            .with_read_only_project();
        let namespace = boundary
            .prepare_view(
                BUBBLEWRAP_PROGRAM,
                std::time::Instant::now() + RETIREMENT_GIT_WAIT,
            )
            .map_err(failure)?;
        let mut producer = namespace
            .host_command(&view, "tar".as_ref())
            .map_err(failure)?
            .args([
                "--acls",
                "--xattrs",
                "--sparse",
                "--exclude=./.git",
                "--exclude=./.exomonad",
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
        #[allow(
            clippy::disallowed_methods,
            reason = "one-shot restoration tar extraction"
        )]
        let consumer = std::process::Command::new("tar")
            .args(["--acls", "--xattrs", "--sparse", "-xf", "-", "-C"])
            .arg(&copy)
            .stdin(input)
            .status();
        if consumer.is_err() {
            producer.kill().ok();
        }
        let produced = producer.wait().map_err(failure)?;
        if !consumer.map_err(failure)?.success() || !produced.success() {
            return Err(failure(std::io::Error::other(
                "retained view restoration copy failed",
            )));
        }
        namespace.detach_retired_tree(&view).map_err(failure)?;
        drop(namespace);
        sync_restored_tree(&copy).map_err(failure)?;
        // A previous interrupted attempt may have left partial ordinary files.
        // They are inaccessible while Retained is registered and can be removed
        // only after a complete new stage is safely on disk.
        for entry in fs::read_dir(&receipt.cwd).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            if entry.file_name() == ".git" || entry.file_name() == ".exomonad" {
                continue;
            }
            if entry.file_type().map_err(failure)?.is_dir() {
                fs::remove_dir_all(entry.path()).map_err(failure)?;
            } else {
                fs::remove_file(entry.path()).map_err(failure)?;
            }
        }
        for entry in fs::read_dir(&copy).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            fs::rename(entry.path(), receipt.cwd.join(entry.file_name())).map_err(failure)?;
        }
        fs::File::open(&receipt.cwd)
            .and_then(|directory| directory.sync_all())
            .map_err(failure)?;
        receipt.status = WorktreeRecordStatus::Finalized;
        self.registry.put(&receipt)?;
        self.registry.release_finalized_manifest(&receipt)?;
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

    /// Directory name, under [`Self::managed_root`], holding the worktrees the
    /// root actor allocates for its OWN use. Children's worktrees are read-only
    /// to the root; the root's own are not, and the two therefore cannot share
    /// one directory when a single read-only bind covers the whole managed root.
    pub const ROOT_ALLOCATION_DIR: &'static str = "root";

    /// The same manager, materializing new worktrees under `worktree_root`
    /// instead of this one's.
    ///
    /// Registry, Git, and source repository are SHARED: a worktree created
    /// through the returned manager is an ordinary registered worktree, so
    /// lookup, observation, merge, retirement, and materialization all reach it
    /// exactly as they reach any other. Only the directory the checkout lands
    /// in differs.
    #[must_use]
    pub fn with_worktree_root(&self, worktree_root: impl Into<PathBuf>) -> Self {
        Self {
            git: self.git.clone(),
            registry: self.registry.clone(),
            worktree_root: worktree_root.into(),
            source_repository: self.source_repository.clone(),
        }
    }

    /// The manager that materializes the ROOT actor's own allocations, in a
    /// directory distinct from every child's so it can be made writable to the
    /// root without also opening children's checkouts to it.
    #[must_use]
    pub fn root_allocations(&self) -> Self {
        self.with_worktree_root(self.worktree_root.join(Self::ROOT_ALLOCATION_DIR))
    }

    /// Repository used by [`WorktreeSource::CurrentRepository`].
    pub fn source_repository(&self) -> &Path {
        &self.source_repository
    }

    /// Adopt the existing checkout identity without changing files, index or HEAD.
    /// Dirty and detached states are valid workspace backings. Operations such
    /// as merge enforce their own working-state requirements when invoked.
    pub fn register_source_checkout(&self) -> Result<WorktreeHandle, WorktreeError> {
        let _adoption = self.registry.adoption.lock().map_err(|_| {
            crate::storage::storage_failure(
                self.registry.root(),
                "workspace adoption lock poisoned",
            )
        })?;
        let source = inspect::work_tree(&self.git, &self.source_repository)?;
        let canonical_source =
            source
                .canonicalize()
                .map_err(|error| WorktreeError::StorageFailure {
                    path: source.clone(),
                    detail: error.to_string(),
                })?;
        // Records only — see `WorktreeRegistry::receipts`. Asking whether THIS
        // checkout is already registered must not depend on the filesystem
        // health of every other retained worktree: a previous run's child whose
        // retirement failed left a `Mounted` receipt whose view this process
        // never installed, and deriving liveness for it turned the root's own
        // `boundWorktree` into `StorageFailure ... requires filesystem recovery`.
        if let Some(receipt) = self
            .registry
            .receipts()?
            .into_iter()
            .find(|receipt| receipt.cwd.canonicalize().ok().as_ref() == Some(&canonical_source))
        {
            return self
                .lookup(&receipt.worktree_id)?
                .ok_or_else(|| WorktreeError::WorktreeNotRegistered(receipt.worktree_id));
        }

        let branch = crate::submission::HeadState::read(&self.git, &canonical_source)?;
        let receipt = WorktreeReceipt {
            worktree_id: self.registry.mint_id()?,
            cwd: canonical_source.clone(),
            branch: branch.branch().cloned(),
            source_head: branch.oid().clone(),
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
        self.create_spec(spec)
    }

    /// Allocate a committed fallback without rejecting or committing dirty files.
    /// Resolve the selected checkout's current HEAD once, then use that exact oid.
    pub fn create_committed_fork(
        &self,
        source: &WorktreeSource,
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
            None,
        )
    }

    /// Prepare a child of the selected live checkout without checking out
    /// files or converting staging into a commit. The source-view owner invokes
    /// this while holding native mutation admission and retains the prepared
    /// checkout until its source mount is installed. Explicit commit seeds use
    /// `create` instead.
    pub fn prepare_inherited_source(
        &self,
        source: &WorktreeSource,
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
            seed: seed.clone(),
            snapshot_ref: None,
            origin,
            git_repository: source.clone(),
            source_repository: source.clone(),
        };
        let handle = self.materialize(self.registry.mint_id()?, resolved, Some(&index))?;
        // The inherited overlay deliberately omits .exomonad. Materialize
        // authored files from the exact checkpoint into this child's durable
        // host checkout before its private .exomonad mount is prepared.
        Self::restore_private_exomonad(&self.git.on_host(), handle.cwd(), &seed)?;
        Self::initialize_submodules(&self.git.on_host(), handle.cwd(), source)?;
        Ok(PreparedSourceWorktree {
            receipt: handle.receipt,
        })
    }

    fn restore_private_exomonad(
        git: &GitCli,
        cwd: &Path,
        seed: &GitOid,
    ) -> Result<(), WorktreeError> {
        for path in [".gitmodules", ".exomonad"] {
            if !git
                .try_run(cwd, &["ls-tree", "--name-only", seed.as_str(), "--", path])?
                .stdout
                .is_empty()
            {
                git.try_run(
                    cwd,
                    &[
                        "restore",
                        "--source",
                        seed.as_str(),
                        "--worktree",
                        "--",
                        path,
                    ],
                )?;
            }
        }
        Ok(())
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
        Self::initialize_submodules(
            &self.git.on_host(),
            &receipt.cwd,
            &receipt.source_repository,
        )?;
        receipt.status = WorktreeRecordStatus::Finalized;
        self.registry.put(&receipt)?;
        Ok(WorktreeHandle::from_receipt(receipt))
    }

    /// Register the child's mounted view after host-side Git preparation. The
    /// mounted view may be read-only, so no Git setup belongs after this point.
    #[cfg(target_os = "linux")]
    pub fn finish_inherited_source(
        &self,
        prepared: PreparedSourceWorktree,
        namespace: exomonad_node::MountNamespace,
        visible_root: &Path,
        layers: Vec<PathBuf>,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let receipt = prepared.receipt;
        if self.registry.get(&receipt.worktree_id)?.as_ref() != Some(&receipt) {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "source preparation does not match this worktree registry".into(),
            ));
        }
        self.registry.put_retained_manifest(&receipt, layers)?;
        let finalized = self
            .registry
            .install_view(&receipt, namespace, visible_root)?;
        Ok(WorktreeHandle::from_receipt(finalized))
    }

    /// Populate every submodule from the gitlinks in this checkout's recorded
    /// commit. Linked worktrees share the superproject's Git metadata, but not
    /// their submodule working directories; without this each child sees empty
    /// workspace paths even though the source commit records the right gitlink.
    fn initialize_submodules(
        git: &GitCli,
        cwd: &Path,
        source_repository: &Path,
    ) -> Result<(), WorktreeError> {
        let workspace_name = if git.try_exists(&cwd.join(".gitmodules"))? {
            let modules = git.try_run(cwd, &["config", "--file", ".gitmodules", "-z", "--list"])?;
            modules.nul_fields().into_iter().find_map(|entry| {
                let (key, path) = entry.split_once('\n')?;
                (path == ".exomonad/workspace")
                    .then_some(key)
                    .and_then(|key| key.strip_prefix("submodule."))
                    .and_then(|key| key.strip_suffix(".path"))
                    .map(str::to_owned)
            })
        } else {
            None
        };
        let local_workspace = source_repository.join(".exomonad/workspace");
        let mut args: Vec<OsString> = Vec::new();
        if let Some(name) = &workspace_name {
            if git.try_exists(&local_workspace.join(".git"))? {
                let mut override_url = OsString::from(format!("submodule.{name}.url="));
                override_url.push(local_workspace.as_os_str());
                args.extend([
                    "-c".into(),
                    override_url,
                    "-c".into(),
                    "protocol.file.allow=always".into(),
                ]);
            }
        }
        args.extend([
            "submodule".into(),
            "update".into(),
            "--init".into(),
            "--recursive".into(),
        ]);
        git.try_run(cwd, &args)?;
        let workspace = cwd.join(".exomonad/workspace");
        let parent_common = inspect::git_common_dir(git, cwd)?;
        let parent_git_dir = inspect::git_dir(git, cwd)?;
        let expected_workspace_common = workspace_name
            .as_ref()
            .map(|name| parent_git_dir.join("modules").join(name));
        Self::normalize_submodule_gitfiles(
            git,
            &workspace,
            &parent_common,
            &parent_git_dir,
            expected_workspace_common.as_deref(),
        )?;
        if let Some(name) = workspace_name.filter(|_| workspace.join(".git").is_file()) {
            // The command-scoped local URL gets an unpublished parent commit
            // into this clone, but Git also records it as origin. Restore the
            // initialized upstream in the child's clone alone.
            let url_key = format!("submodule.{name}.url");
            let upstream = git.try_run(cwd, &["config", "--local", "--get", &url_key])?;
            let _workspace = git.write_scope(&workspace)?;
            Self::require_repository_root(git, &workspace, &workspace)?;
            let expected_common = parent_git_dir.join("modules").join(&name);
            Self::require_repository_common_dir(git, &workspace, &expected_common)?;
            git.try_run(
                &workspace,
                &["remote", "set-url", "origin", upstream.trimmed()],
            )?;
        }
        Ok(())
    }

    fn normalize_submodule_gitfiles(
        git: &GitCli,
        workspace: &Path,
        parent_common: &Path,
        parent_git_dir: &Path,
        expected_workspace_common: Option<&Path>,
    ) -> Result<(), WorktreeError> {
        if !workspace.is_dir() {
            return Ok(());
        }
        // This host-backed subtree is mounted at a stable actor path. Relative
        // gitdir pointers written by Git at the host path would resolve from a
        // different parent in that view. Embedded .git directories stay intact.
        let mut pending = vec![(
            workspace.to_path_buf(),
            vec![parent_common.to_path_buf(), parent_git_dir.to_path_buf()],
        )];
        while let Some((directory, ancestor_common_dirs)) = pending.pop() {
            let mut child_common_dirs = ancestor_common_dirs.clone();
            let mut child_directories = Vec::new();
            for entry in std::fs::read_dir(&directory)
                .map_err(|error| crate::storage::storage_failure(&directory, error))?
            {
                let entry =
                    entry.map_err(|error| crate::storage::storage_failure(&directory, error))?;
                let kind = entry
                    .file_type()
                    .map_err(|error| crate::storage::storage_failure(&entry.path(), error))?;
                if entry.file_name() == ".git" {
                    if kind.is_file() || kind.is_dir() {
                        let _repository = git.write_scope(&directory)?;
                        Self::require_repository_root(git, &directory, &directory)?;
                        let admin = inspect::git_dir(git, &directory)?;
                        let common = inspect::git_common_dir(git, &directory)?;
                        let canonical_common = fs::canonicalize(&common)
                            .map_err(|error| crate::storage::storage_failure(&common, error))?;
                        for ancestor in &ancestor_common_dirs {
                            let canonical_ancestor =
                                fs::canonicalize(ancestor).map_err(|error| {
                                    crate::storage::storage_failure(ancestor, error)
                                })?;
                            if canonical_ancestor == canonical_common {
                                return Err(WorktreeError::GitRepositoryIdentityMismatch {
                                    path: directory.clone(),
                                    detail: format!(
                                        "Git metadata {} aliases ancestor metadata {}",
                                        canonical_common.display(),
                                        canonical_ancestor.display()
                                    ),
                                });
                            }
                        }
                        if kind.is_file() && directory == workspace {
                            if let Some(expected) = expected_workspace_common {
                                Self::require_repository_common_dir(git, &directory, expected)?;
                            }
                        }
                        child_common_dirs.push(admin.clone());
                        child_common_dirs.push(common);
                        if kind.is_file() {
                            let pointer = format!("gitdir: {}\n", admin.display());
                            tidepool_atomic_write::write_best_effort(
                                &entry.path(),
                                pointer.as_bytes(),
                            )
                            .map_err(|error| {
                                crate::storage::storage_failure(&error.path, error.source)
                            })?;
                        }
                    }
                } else if kind.is_dir() {
                    child_directories.push(entry.path());
                }
            }
            pending.extend(
                child_directories
                    .into_iter()
                    .map(|child| (child, child_common_dirs.clone())),
            );
        }
        Ok(())
    }

    fn require_repository_root(
        git: &GitCli,
        cwd: &Path,
        expected_root: &Path,
    ) -> Result<(), WorktreeError> {
        let actual = inspect::work_tree(git, cwd)?;
        let canonical_actual = fs::canonicalize(&actual)
            .map_err(|error| crate::storage::storage_failure(&actual, error))?;
        let canonical_expected = fs::canonicalize(expected_root)
            .map_err(|error| crate::storage::storage_failure(expected_root, error))?;
        if canonical_actual != canonical_expected {
            return Err(WorktreeError::GitRepositoryIdentityMismatch {
                path: cwd.to_path_buf(),
                detail: format!(
                    "Git repository root {} does not match expected child {}",
                    canonical_actual.display(),
                    canonical_expected.display()
                ),
            });
        }
        Ok(())
    }

    fn require_repository_common_dir(
        git: &GitCli,
        cwd: &Path,
        expected_common: &Path,
    ) -> Result<(), WorktreeError> {
        let actual = inspect::git_common_dir(git, cwd)?;
        let canonical_actual = fs::canonicalize(&actual)
            .map_err(|error| crate::storage::storage_failure(&actual, error))?;
        let canonical_expected = fs::canonicalize(expected_common)
            .map_err(|error| crate::storage::storage_failure(expected_common, error))?;
        if canonical_actual != canonical_expected {
            return Err(WorktreeError::GitRepositoryIdentityMismatch {
                path: cwd.to_path_buf(),
                detail: format!(
                    "Git repository metadata {} does not match expected submodule metadata {}",
                    canonical_actual.display(),
                    canonical_expected.display()
                ),
            });
        }
        Ok(())
    }

    /// Bind a completed checkout to its retained launch view, or reattach that
    /// view after recovery. Verify the registered Git identity without resetting
    /// HEAD, the index, or working files. Native execution and mount construction
    /// remain with their resource owners.
    #[cfg(target_os = "linux")]
    pub fn mount_worktree(
        &self,
        id: &WorktreeId,
        namespace: exomonad_node::MountNamespace,
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
        expected: &exomonad_node::MountNamespace,
        namespace: exomonad_node::MountNamespace,
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

    fn create_spec(&self, spec: &WorktreeSpec) -> Result<WorktreeHandle, WorktreeError> {
        let id = self.registry.mint_id()?;
        let resolved = self.resolve_source(spec, &id)?;

        self.materialize(id, resolved, None)
    }

    fn materialize(
        &self,
        id: WorktreeId,
        resolved: ResolvedSeed,
        inherited_index: Option<&Path>,
    ) -> Result<WorktreeHandle, WorktreeError> {
        let host_git = self.git.on_host();
        let canonical_root = validate_external_root(&host_git, &self.worktree_root)?;
        let common = inspect::git_common_dir(&self.git, &resolved.git_repository)?;
        let _write = host_git.write_scope(&common)?;
        fs::create_dir_all(&canonical_root).map_err(|error| WorktreeError::StorageFailure {
            path: self.worktree_root.clone(),
            detail: error.to_string(),
        })?;
        let cwd = canonical_root.join(id.as_str());
        let branch = BranchName::from_raw(format!("{EXOMONAD_BRANCH_PREFIX}/{}", id.as_str()));

        let provisional = WorktreeReceipt {
            worktree_id: id.clone(),
            cwd: cwd.clone(),
            branch: Some(branch.clone()),
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
        host_git.try_run(&common, &args)?;
        // The new checkout shares the common lock, but its working tree and
        // private Git directory have distinct identities. Admit that view
        // explicitly before operating on its index or initializing it.
        let _new_worktree = host_git.write_scope(&cwd)?;

        if let Some(index) = inherited_index {
            let directory = inspect::git_dir(&host_git, &cwd)?;
            fs::copy(index, directory.join("index"))
                .map_err(|error| crate::storage::storage_failure(&directory, error))?;
            // Working files are not installed yet. Keep the durable receipt
            // provisional until the source-view owner completes that step.
            return Ok(WorktreeHandle::from_receipt(provisional));
        }

        Self::initialize_submodules(&host_git, &cwd, &resolved.source_repository)?;

        let finalized = WorktreeReceipt {
            status: WorktreeRecordStatus::Finalized,
            ..provisional
        };
        self.registry.put(&finalized)?;

        Ok(WorktreeHandle::from_receipt(finalized))
    }

    fn prepare_worktree_root(&self) -> Result<(), WorktreeError> {
        let canonical_root = validate_external_root(&self.git, &self.worktree_root)?;
        fs::create_dir_all(&canonical_root).map_err(|e| WorktreeError::StorageFailure {
            path: self.worktree_root.clone(),
            detail: e.to_string(),
        })?;
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
            Some(mut receipt) => {
                if receipt.status == WorktreeRecordStatus::Retained {
                    #[cfg(target_os = "linux")]
                    self.restore_retained_view(id)?;
                    #[cfg(not(target_os = "linux"))]
                    return Err(WorktreeError::WorktreeAuthorityDenied(
                        "retained checkout restoration requires Linux".into(),
                    ));
                    receipt = self
                        .registry
                        .get(id)?
                        .ok_or_else(|| WorktreeError::WorktreeNotRegistered(id.clone()))?;
                }
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

    /// Git administration remains host-owned when working files are retained
    /// in layers. These two read-only observations need no full checkout copy.
    pub fn worktree_head_by_id(&self, id: &WorktreeId) -> Result<Option<GitOid>, WorktreeError> {
        let Some(receipt) = self.registry.get(id)? else {
            return Ok(None);
        };
        if receipt.status == WorktreeRecordStatus::Retained {
            self.registry.retained_manifest(&receipt)?.ok_or_else(|| {
                crate::storage::storage_failure(&receipt.cwd, "retained view manifest is missing")
            })?;
            if !self.git.on_host().try_exists(&receipt.cwd.join(".git"))? {
                return Err(WorktreeError::WorktreeLost(id.clone()));
            }
            let out = self
                .git
                .on_host()
                .try_run(&receipt.cwd, &["rev-parse", "HEAD"])?;
            return Ok(Some(GitOid::from_raw(out.trimmed())));
        }
        self.lookup(id)?
            .map(|handle| self.worktree_head(&handle))
            .transpose()
    }

    pub fn worktree_branch_by_id(
        &self,
        id: &WorktreeId,
    ) -> Result<Option<BranchName>, WorktreeError> {
        let Some(receipt) = self.registry.get(id)? else {
            return Ok(None);
        };
        if receipt.status == WorktreeRecordStatus::Retained {
            self.registry.retained_manifest(&receipt)?.ok_or_else(|| {
                crate::storage::storage_failure(&receipt.cwd, "retained view manifest is missing")
            })?;
            if !self.git.on_host().try_exists(&receipt.cwd.join(".git"))? {
                return Err(WorktreeError::WorktreeLost(id.clone()));
            }
            let out = self
                .git
                .on_host()
                .try_run(&receipt.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
            return Ok(Some(BranchName::from_raw(out.trimmed())));
        }
        self.lookup(id)?
            .map(|handle| {
                self.git
                    .try_run(handle.cwd(), &["rev-parse", "--abbrev-ref", "HEAD"])
                    .map(|out| BranchName::from_raw(out.trimmed()))
            })
            .transpose()
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

#[cfg(test)]
mod submodule_gitfile_tests {
    use super::*;
    use crate::testing::TestRepo;

    #[test]
    fn refuses_workspace_gitfile_that_resolves_to_parent_metadata() {
        let parent = TestRepo::init().unwrap();
        parent
            .git()
            .try_run(
                parent.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.invalid/root.git",
                ],
            )
            .unwrap();
        parent
            .writer()
            .commit_file(
                ".gitmodules",
                "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = /tmp/not-used\n",
                "declare workspace submodule",
            )
            .unwrap();
        let workspace = parent.path().join(".exomonad/workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", parent.path().join(".git").display()),
        )
        .unwrap();

        let before = parent
            .git()
            .try_run(parent.path(), &["remote", "get-url", "origin"])
            .unwrap();
        let failure =
            WorktreeManager::initialize_submodules(parent.git(), parent.path(), parent.path())
                .unwrap_err();

        assert!(matches!(
            failure,
            WorktreeError::GitRepositoryIdentityMismatch { .. }
        ));
        let after = parent
            .git()
            .try_run(parent.path(), &["remote", "get-url", "origin"])
            .unwrap();
        assert_eq!(after.trimmed(), before.trimmed());
    }

    #[test]
    fn normalizes_valid_linked_workspace_submodule_without_touching_parent_origin() {
        let child = TestRepo::init().unwrap();
        child
            .writer()
            .commit_file("README", "child\n", "child seed")
            .unwrap();

        let parent = TestRepo::init().unwrap();
        parent
            .git()
            .try_run(
                parent.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.invalid/root.git",
                ],
            )
            .unwrap();
        parent
            .git()
            .try_run(
                parent.path(),
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    child.path().to_str().unwrap(),
                    ".exomonad/workspace",
                ],
            )
            .unwrap();
        parent
            .writer()
            .commit_file("root.txt", "root\n", "parent seed")
            .unwrap();

        let target_root = tempfile::tempdir().unwrap();
        let target = target_root.path().join("target");
        parent
            .git()
            .try_run(
                parent.path(),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "test/submodule-target",
                    target.to_str().unwrap(),
                    "HEAD",
                ],
            )
            .unwrap();
        let before = parent
            .git()
            .try_run(parent.path(), &["remote", "get-url", "origin"])
            .unwrap();

        WorktreeManager::initialize_submodules(parent.git(), &target, parent.path()).unwrap();

        let workspace = target.join(".exomonad/workspace");
        assert_eq!(
            fs::canonicalize(inspect::work_tree(parent.git(), &workspace).unwrap()).unwrap(),
            fs::canonicalize(&workspace).unwrap()
        );
        let expected_common = inspect::git_dir(parent.git(), &target)
            .unwrap()
            .join("modules/.exomonad/workspace");
        assert_eq!(
            fs::canonicalize(inspect::git_common_dir(parent.git(), &workspace).unwrap()).unwrap(),
            fs::canonicalize(expected_common).unwrap()
        );
        let after = parent
            .git()
            .try_run(parent.path(), &["remote", "get-url", "origin"])
            .unwrap();
        assert_eq!(after.trimmed(), before.trimmed());
    }

    #[test]
    fn refuses_nested_gitfile_aliasing_embedded_workspace_metadata() {
        let parent = TestRepo::init().unwrap();
        parent
            .git()
            .try_run(
                parent.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.invalid/root.git",
                ],
            )
            .unwrap();
        parent
            .writer()
            .commit_file(
                ".gitmodules",
                "[submodule \"workspace\"]\n\tpath = .exomonad/workspace\n\turl = /tmp/not-used\n",
                "declare workspace submodule",
            )
            .unwrap();

        let workspace = parent.path().join(".exomonad/workspace");
        fs::create_dir_all(&workspace).unwrap();
        parent
            .git()
            .init_repository(&workspace, &["--initial-branch=main", "-q"])
            .unwrap();
        for (key, value) in [
            ("user.name", "Embedded Workspace"),
            ("user.email", "workspace@example.invalid"),
            ("remote.origin.url", "https://example.invalid/workspace.git"),
        ] {
            parent
                .git()
                .try_run(&workspace, &["config", key, value])
                .unwrap();
        }
        parent
            .writer_at(&workspace)
            .commit_file("README", "workspace\n", "workspace seed")
            .unwrap();

        let nested = workspace.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let nested_gitfile = nested.join(".git");
        let nested_gitfile_contents = format!("gitdir: {}\n", workspace.join(".git").display());
        fs::write(&nested_gitfile, &nested_gitfile_contents).unwrap();
        let parent_origin_before = parent
            .git()
            .try_run(parent.path(), &["remote", "get-url", "origin"])
            .unwrap();
        let workspace_origin_before = parent
            .git()
            .try_run(&workspace, &["remote", "get-url", "origin"])
            .unwrap();

        let failure =
            WorktreeManager::initialize_submodules(parent.git(), parent.path(), parent.path())
                .unwrap_err();

        assert!(matches!(
            failure,
            WorktreeError::GitRepositoryIdentityMismatch { .. }
        ));
        assert_eq!(
            fs::read_to_string(&nested_gitfile).unwrap(),
            nested_gitfile_contents
        );
        assert_eq!(
            parent
                .git()
                .try_run(parent.path(), &["remote", "get-url", "origin"])
                .unwrap()
                .trimmed(),
            parent_origin_before.trimmed()
        );
        assert_eq!(
            parent
                .git()
                .try_run(&workspace, &["remote", "get-url", "origin"])
                .unwrap()
                .trimmed(),
            workspace_origin_before.trimmed()
        );
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
