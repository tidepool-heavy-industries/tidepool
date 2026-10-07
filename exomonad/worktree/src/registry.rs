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
//! - **Restart retains filesystem requirements.** Mounted receipts require their
//!   exact retained view to be recovered before filesystem operations can resume.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid, GitRef, WorktreeId};
use crate::storage::{storage_failure, DurableJsonDir};
use tidepool_atomic_write::DirectoryAnchor;

/// Directory under the registry root holding one JSON file per worktree id.
const RECORDS_DIR: &str = "records";
const RETAINED_DIR: &str = "retained-views";

/// Whether the recorded `cwd` still holds a real git working tree. A plain
/// `Path::exists` would be fooled by a directory left behind with its `.git`
/// file removed; this reconciles like everything else in this crate.
pub(crate) fn worktree_present(git: &GitCli, cwd: &Path) -> Result<bool, WorktreeError> {
    if !git.try_exists(&cwd.join(".git"))? {
        return Ok(false);
    }
    match inspect::work_tree(git, cwd) {
        Ok(_) => Ok(true),
        Err(WorktreeError::NotARepository(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Resolve a possibly not-yet-created storage root and refuse any Git working
/// tree before callers create it. Canonicalizing the nearest existing ancestor
/// preserves symlink resolution without materializing the candidate first.
pub(crate) fn validate_external_root(git: &GitCli, root: &Path) -> Result<PathBuf, WorktreeError> {
    let absolute = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| storage_failure(root, error))?
            .join(root)
    };
    let mut candidate = PathBuf::new();
    let mut missing = Vec::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(prefix) => candidate.push(prefix.as_os_str()),
            std::path::Component::RootDir => candidate.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if !missing.is_empty() => {
                return Err(WorktreeError::StorageFailure {
                    path: root.to_owned(),
                    detail: "parent traversal after a missing storage-root component is ambiguous"
                        .into(),
                });
            }
            std::path::Component::ParentDir => {
                candidate.pop();
            }
            std::path::Component::Normal(name) if missing.is_empty() => {
                let next = candidate.join(name);
                match fs::symlink_metadata(&next) {
                    Ok(_) => {
                        candidate = next
                            .canonicalize()
                            .map_err(|error| storage_failure(&next, error))?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        missing.push(name.to_owned());
                    }
                    Err(error) => return Err(storage_failure(&next, error)),
                }
            }
            std::path::Component::Normal(name) => missing.push(name.to_owned()),
        }
    }
    let existing_ancestor = if candidate.is_dir() {
        candidate.clone()
    } else {
        candidate.parent().unwrap_or(&candidate).to_owned()
    };
    for component in missing {
        candidate.push(component);
    }
    match inspect::work_tree(git, &existing_ancestor) {
        Ok(toplevel) => {
            let canonical_toplevel = toplevel
                .canonicalize()
                .map_err(|error| storage_failure(&toplevel, error))?;
            if candidate.starts_with(&canonical_toplevel) {
                return Err(WorktreeError::InvalidRegistryRoot {
                    root: candidate,
                    inside: canonical_toplevel,
                });
            }
        }
        Err(WorktreeError::NotARepository(_)) => {}
        Err(other) => return Err(other),
    }
    Ok(candidate)
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

/// Checkout readiness and the filesystem required to access its working files.
/// Preparation records `Provisional` before Git creation; completion records
/// `Finalized` for ordinary host files or `Mounted` for an inherited source view.
/// Retirement seals the source view as `Retained` until demand restores it.
/// Provisional storage remains discoverable but cannot grant a usable handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeRecordStatus {
    Provisional,
    Finalized,
    /// Complete checkout whose working files require its retained mount view.
    /// Older readers reject this variant instead of inspecting a Git-only host directory.
    Mounted,
    /// Working files live in durable overlay layers and are restored on demand.
    Retained,
}

/// Durable custody of the exact source view at a retired worktree's boundary.
/// Paths are validated against the managed storage root on both write and read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedViewManifest {
    pub version: u32,
    pub worktree_id: WorktreeId,
    pub cwd: PathBuf,
    /// Overlay lower layers in oldest-to-newest order, then the sealed upper.
    pub layers: Vec<PathBuf>,
    /// Both alternatives remain authoritative while a rotation's mount result
    /// is unconfirmed. Recovery refuses to choose one by inspecting files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_layers: Option<Vec<PathBuf>>,
}

/// The durable registry row for one managed worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeReceipt {
    pub worktree_id: WorktreeId,
    /// Absolute path of the managed working tree. Outside the source tree.
    ///
    /// Readable by the worktree's OWNER. A parent reads a child's commits
    /// through the shared Git namespace (`git show <oid>`) and through typed
    /// observation, not through this path: a running child's working files live
    /// behind its own retained mount view, and they become readable at `cwd` to
    /// anyone else only after [`crate::WorktreeManager::restore_retained_view`]
    /// has materialized the sealed source layers on demand.
    pub cwd: PathBuf,
    pub branch: Option<BranchName>,
    /// The commit the managed branch was rooted at. For a snapshot creation
    /// this is the synthetic snapshot commit, not the pre-snapshot source
    /// `HEAD` (which `snapshot_ref` lets you reach via its parent).
    pub source_head: GitOid,
    /// `Some` exactly when the tree was created through
    /// [`crate::snapshot`] — the Tidepool-owned ref holding the synthetic
    /// snapshot commit. `None` when rooted at an actual source commit, including
    /// live source inheritance that preserves uncommitted changes separately.
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
    /// For an ordinary checkout, whether the recorded `cwd` still holds a Git
    /// worktree. For `Retained`, whether its durable layer manifest is valid;
    /// the sparse `cwd` is restored only by lookup.
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
    retained: DurableJsonDir,
    pub(crate) adoption: std::sync::Arc<std::sync::Mutex<()>>,
    #[cfg(target_os = "linux")]
    pub(crate) views: crate::view::WorktreeViews,
}

impl WorktreeRegistry {
    /// Establish a registry below the caller's stable project-storage boundary.
    /// Refuse roots inside any Git working tree before creating directories.
    pub fn open(
        anchor: &DirectoryAnchor,
        relative: impl AsRef<Path>,
    ) -> Result<Self, WorktreeError> {
        let relative = relative.as_ref();
        let root = anchor
            .resolve(relative)
            .map_err(|e| storage_failure(&e.path, e.source))?;
        let git = GitCli::new();
        validate_external_root(&git, &root)?;
        let registry_anchor = anchor
            .child(relative)
            .map_err(|e| storage_failure(&e.path, e.source))?;
        let canonical_root = registry_anchor.path().to_path_buf();
        let records = DurableJsonDir::open(&registry_anchor, RECORDS_DIR)?;
        let retained = DurableJsonDir::open(&registry_anchor, RETAINED_DIR)?;

        let registry = Self {
            root: canonical_root,
            records,
            retained,
            adoption: Default::default(),
            #[cfg(target_os = "linux")]
            views: crate::view::WorktreeViews::default(),
        };
        registry.read_receipts()?;
        Ok(registry)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The manifest is stored before the receipt changes to `Retained`.
    /// An orphan manifest is safe to keep; a retained receipt without a valid
    /// manifest never authorizes filesystem access or cleanup.
    pub fn put_retained_manifest(
        &self,
        receipt: &WorktreeReceipt,
        layers: Vec<PathBuf>,
    ) -> Result<RetainedViewManifest, WorktreeError> {
        let manifest = RetainedViewManifest {
            version: 1,
            worktree_id: receipt.worktree_id.clone(),
            cwd: receipt.cwd.clone(),
            layers,
            pending_layers: None,
        };
        self.validate_retained_manifest(receipt, &manifest)?;
        #[allow(clippy::expect_used, reason = "serialize RetainedViewManifest")]
        let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize RetainedViewManifest");
        self.retained.write(receipt.worktree_id.as_str(), &bytes)?;
        Ok(manifest)
    }

    /// Replace a live descriptor only while its exact mounted receipt is
    /// current. Publication owns the gate excluding writes and rotations.
    pub fn put_mounted_layers(
        &self,
        receipt: &WorktreeReceipt,
        layers: Vec<PathBuf>,
    ) -> Result<(), WorktreeError> {
        if receipt.status != WorktreeRecordStatus::Mounted
            || self.get(&receipt.worktree_id)?.as_ref() != Some(receipt)
        {
            return Err(storage_failure(
                &receipt.cwd,
                "source descriptor requires the current Mounted receipt",
            ));
        }
        self.put_retained_manifest(receipt, layers).map(|_| ())
    }

    /// Persist both candidate views before a live source rotation can alter
    /// its mount. The publication owner later writes the confirmed stable
    /// descriptor while writers are still excluded.
    pub fn put_mounted_transition(
        &self,
        receipt: &WorktreeReceipt,
        before: Vec<PathBuf>,
        after: Vec<PathBuf>,
    ) -> Result<(), WorktreeError> {
        if receipt.status != WorktreeRecordStatus::Mounted
            || self.get(&receipt.worktree_id)?.as_ref() != Some(receipt)
        {
            return Err(storage_failure(
                &receipt.cwd,
                "source transition requires Mounted",
            ));
        }
        let manifest = RetainedViewManifest {
            version: 1,
            worktree_id: receipt.worktree_id.clone(),
            cwd: receipt.cwd.clone(),
            layers: before,
            pending_layers: Some(after),
        };
        self.validate_retained_manifest(receipt, &manifest)?;
        #[allow(clippy::expect_used, reason = "serialize RetainedViewManifest")]
        let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize RetainedViewManifest");
        self.retained.write(receipt.worktree_id.as_str(), &bytes)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn finish_retention(&self, receipt: &WorktreeReceipt) -> Result<(), WorktreeError> {
        let manifest = self.retained_manifest(receipt)?;
        if receipt.status != WorktreeRecordStatus::Retained
            || manifest
                .as_ref()
                .is_none_or(|manifest| manifest.pending_layers.is_some())
        {
            return Err(storage_failure(
                &receipt.cwd,
                "retained receipt lacks a valid manifest",
            ));
        }
        #[allow(clippy::expect_used, reason = "serialize WorktreeReceipt")]
        let bytes = serde_json::to_vec_pretty(receipt).expect("serialize WorktreeReceipt");
        self.records.write(receipt.worktree_id.as_str(), &bytes)?;
        self.views
            .retain(&receipt.cwd)
            .map_err(|error| storage_failure(&receipt.cwd, error))
    }

    pub fn retained_manifest(
        &self,
        receipt: &WorktreeReceipt,
    ) -> Result<Option<RetainedViewManifest>, WorktreeError> {
        let Some(bytes) = self.retained.read(receipt.worktree_id.as_str())? else {
            return Ok(None);
        };
        let manifest: RetainedViewManifest = serde_json::from_slice(&bytes).map_err(|error| {
            storage_failure(&self.retained.path_for(receipt.worktree_id.as_str()), error)
        })?;
        self.validate_retained_manifest(receipt, &manifest)?;
        Ok(Some(manifest))
    }

    /// A fully materialized, fsynced checkout no longer needs its layer
    /// custody. The Finalized receipt is written first; a crash before this
    /// unlink leaves a harmless stale manifest that cleanup ignores.
    pub fn release_finalized_manifest(
        &self,
        receipt: &WorktreeReceipt,
    ) -> Result<(), WorktreeError> {
        if receipt.status != WorktreeRecordStatus::Finalized
            || self.get(&receipt.worktree_id)?.as_ref() != Some(receipt)
        {
            return Err(storage_failure(
                &receipt.cwd,
                "manifest release requires the exact finalized receipt",
            ));
        }
        let path = self.retained.path_for(receipt.worktree_id.as_str());
        match fs::remove_file(&path) {
            Ok(()) => tidepool_atomic_write::sync_parent_directory(&path)
                .map_err(|error| storage_failure(&error.path, error.source)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(storage_failure(&path, error)),
        }
    }

    /// Return all source layers that offline cleanup must retain. `None`
    /// means a mounted checkout lacks a durable descriptor, so ancestry is
    /// unknown and no source resource in this repository can be reclaimed.
    /// Orphan manifests on Mounted receipts pin layers too: they can result
    /// from interruption between manifest publication and receipt transition.
    /// A stale manifest on a Finalized receipt is no longer a live reference.
    pub fn source_layer_references(&self) -> Result<Option<BTreeSet<PathBuf>>, WorktreeError> {
        let receipts = self.read_receipts()?;
        let mut layers = BTreeSet::new();
        for receipt in &receipts {
            if receipt.status == WorktreeRecordStatus::Mounted
                && !self.retained.exists(receipt.worktree_id.as_str())
            {
                return Ok(None);
            }
            if receipt.status == WorktreeRecordStatus::Retained
                && !self.retained.exists(receipt.worktree_id.as_str())
            {
                return Err(storage_failure(
                    &receipt.cwd,
                    "retained view manifest is missing",
                ));
            }
        }
        for (path, bytes) in self.retained.read_all()? {
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| storage_failure(&path, "invalid retained manifest filename"))?;
            let receipt = receipts
                .iter()
                .find(|receipt| receipt.worktree_id.as_str() == id)
                .ok_or_else(|| storage_failure(&path, "manifest lacks a worktree receipt"))?;
            if receipt.status == WorktreeRecordStatus::Finalized {
                continue;
            }
            let manifest: RetainedViewManifest =
                serde_json::from_slice(&bytes).map_err(|error| storage_failure(&path, error))?;
            self.validate_retained_manifest(receipt, &manifest)?;
            layers.extend(manifest.layers);
            if let Some(pending) = manifest.pending_layers {
                layers.extend(pending);
            }
        }
        Ok(Some(layers))
    }

    fn validate_retained_manifest(
        &self,
        receipt: &WorktreeReceipt,
        manifest: &RetainedViewManifest,
    ) -> Result<(), WorktreeError> {
        let storage_root = self
            .root
            .parent()
            .ok_or_else(|| storage_failure(&self.root, "registry has no managed storage root"))?;
        let resource_root = storage_root.join("worktrees/.resources");
        if manifest.version != 1
            || manifest.worktree_id != receipt.worktree_id
            || manifest.cwd != receipt.cwd
            || manifest.layers.is_empty()
        {
            return Err(storage_failure(
                &self.root,
                "invalid retained-view identity or version",
            ));
        }
        for layer in manifest
            .layers
            .iter()
            .chain(manifest.pending_layers.iter().flatten())
        {
            let canonical = layer
                .canonicalize()
                .map_err(|error| storage_failure(layer, error))?;
            if canonical != *layer || !canonical.starts_with(&resource_root) || !canonical.is_dir()
            {
                return Err(storage_failure(
                    layer,
                    "retained layer is outside managed source resources or is not a directory",
                ));
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn install_view(
        &self,
        receipt: &WorktreeReceipt,
        namespace: exomonad_node::MountNamespace,
        visible_root: &Path,
    ) -> Result<WorktreeReceipt, WorktreeError> {
        self.install_view_checked(receipt, namespace, visible_root, None)
    }

    pub(crate) fn activate_view(
        &self,
        receipt: &WorktreeReceipt,
        expected: &exomonad_node::MountNamespace,
        namespace: exomonad_node::MountNamespace,
        visible_root: &Path,
    ) -> Result<WorktreeReceipt, WorktreeError> {
        self.install_view_checked(receipt, namespace, visible_root, Some(expected))
    }

    fn install_view_checked(
        &self,
        receipt: &WorktreeReceipt,
        namespace: exomonad_node::MountNamespace,
        visible_root: &Path,
        expected: Option<&exomonad_node::MountNamespace>,
    ) -> Result<WorktreeReceipt, WorktreeError> {
        if !visible_root.is_absolute()
            || visible_root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(storage_failure(
                visible_root,
                "invalid mounted worktree root",
            ));
        }
        let expected_git = inspect::git_dir(&GitCli::new(), &receipt.cwd)?;
        let mounted_git = GitCli::new().with_mount_namespace(namespace.clone());
        if inspect::git_dir(&mounted_git, visible_root)? != expected_git {
            return Err(WorktreeError::WorktreeAuthorityDenied(
                "mounted source view belongs to a different Git worktree".into(),
            ));
        }
        if receipt.status == WorktreeRecordStatus::Provisional {
            if self
                .retained_manifest(receipt)?
                .as_ref()
                .is_none_or(|manifest| manifest.pending_layers.is_some())
            {
                return Err(storage_failure(
                    &receipt.cwd,
                    "mounted view requires a stable durable source descriptor",
                ));
            }
            let head = mounted_git.try_run(visible_root, &["rev-parse", "HEAD"])?;
            let branch = mounted_git.try_run(visible_root, &["symbolic-ref", "--short", "HEAD"])?;
            if head.trimmed() != receipt.source_head.as_str()
                || Some(branch.trimmed()) != receipt.branch.as_ref().map(BranchName::as_str)
            {
                return Err(WorktreeError::WorktreeAuthorityDenied(
                    "prepared source Git state changed before finalization".into(),
                ));
            }
        }
        let mounted = WorktreeReceipt {
            status: if receipt.status == WorktreeRecordStatus::Provisional {
                WorktreeRecordStatus::Mounted
            } else {
                receipt.status
            },
            ..receipt.clone()
        };
        // Install access before publishing Mounted so concurrent readers never
        // observe a completed receipt without its retained filesystem view.
        let view = crate::view::MountedView {
            namespace,
            root: visible_root.to_owned(),
        };
        match expected {
            Some(expected) => self.views.activate(&receipt.cwd, expected, view),
            None => self.views.install(&receipt.cwd, view),
        }
        .map_err(|error| storage_failure(&receipt.cwd, error))?;
        // Only inherited source depends on mounts for its working files.
        // Host-backed checkouts remain usable from Git after this wave ends.
        if receipt.status != mounted.status {
            self.put(&mounted)?;
        }
        Ok(mounted)
    }

    /// Durably record a receipt. Overwrites an existing row for the same id
    /// (the snapshot lane writes `snapshot_ref` after creation).
    pub fn put(&self, receipt: &WorktreeReceipt) -> Result<(), WorktreeError> {
        if receipt.status != WorktreeRecordStatus::Finalized {
            self.require_view_if_needed(receipt)?;
        }
        #[allow(clippy::expect_used, reason = "serialize WorktreeReceipt")]
        let bytes = serde_json::to_vec_pretty(receipt).expect("serialize WorktreeReceipt");
        self.records.write(receipt.worktree_id.as_str(), &bytes)?;
        if receipt.status == WorktreeRecordStatus::Finalized {
            self.require_view_if_needed(receipt)?;
        }
        Ok(())
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
        self.require_view_if_needed(&receipt)?;
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
        self.list_with_git(&GitCli::new())
    }

    pub(crate) fn list_with_git(
        &self,
        git: &GitCli,
    ) -> Result<Vec<WorktreeSummary>, WorktreeError> {
        #[cfg(target_os = "linux")]
        let git = &git.clone().with_worktree_views(self.views.clone());
        let mut receipts = self.read_receipts()?;
        receipts.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.worktree_id.cmp(&b.worktree_id))
        });
        receipts
            .into_iter()
            .map(|receipt| {
                let present = if receipt.status == WorktreeRecordStatus::Retained {
                    self.retained_manifest(&receipt)?.ok_or_else(|| {
                        storage_failure(&receipt.cwd, "retained view manifest is missing")
                    })?;
                    true
                } else {
                    worktree_present(git, &receipt.cwd)?
                };
                Ok(WorktreeSummary { receipt, present })
            })
            .collect()
    }

    /// Every recorded receipt, WITHOUT deriving liveness.
    ///
    /// [`Self::list`] answers "what is retained, and is each one still there",
    /// and the liveness half runs Git inside every recorded checkout — so one
    /// worktree whose retained mount view is gone (a previous run's child whose
    /// retirement failed) makes the whole listing fail. A caller that only
    /// needs the RECORDS — which checkout is already registered as the source,
    /// say — must not be held hostage by an unrelated worktree's filesystem.
    /// Nothing is skipped or hidden here: a corrupt record still fails loud,
    /// and `list` still reports the unavailable view to whoever asked about it.
    pub(crate) fn receipts(&self) -> Result<Vec<WorktreeReceipt>, WorktreeError> {
        self.read_receipts()
    }

    fn read_receipts(&self) -> Result<Vec<WorktreeReceipt>, WorktreeError> {
        let mut receipts = Vec::new();
        for (path, bytes) in self.records.read_all()? {
            let receipt: WorktreeReceipt =
                serde_json::from_slice(&bytes).map_err(|e| storage_failure(&path, e))?;
            self.require_view_if_needed(&receipt)?;
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    fn require_view_if_needed(&self, receipt: &WorktreeReceipt) -> Result<(), WorktreeError> {
        if receipt.status == WorktreeRecordStatus::Mounted {
            #[cfg(target_os = "linux")]
            self.views
                .require(&receipt.cwd)
                .map_err(|error| storage_failure(&receipt.cwd, error))?;
        }
        if receipt.status == WorktreeRecordStatus::Retained {
            #[cfg(target_os = "linux")]
            self.views
                .retain(&receipt.cwd)
                .map_err(|error| storage_failure(&receipt.cwd, error))?;
        }
        if receipt.status == WorktreeRecordStatus::Finalized {
            #[cfg(target_os = "linux")]
            self.views
                .clear_restored(&receipt.cwd)
                .map_err(|error| storage_failure(&receipt.cwd, error))?;
        }
        Ok(())
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
