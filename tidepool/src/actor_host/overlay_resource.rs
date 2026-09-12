//! Shared overlay publication and storage custody for source and build views.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_node::{
    MountNamespace, OverlayRecovery, OverlayRotation, OverlayRotationOutcome, ProcessBoundaryError,
    ProcessMountBoundary,
};

#[derive(Debug)]
pub(super) struct OverlayResourceLease {
    storage: Arc<OverlayStorage>,
    layers: Vec<OverlayLayer>,
    upper: PathBuf,
    work: PathBuf,
    latest: Arc<Mutex<Option<OverlaySnapshot>>>,
    publication: PublicationState,
    empty_upper: Option<SourceStamp>,
    custody: CustodyGuard,
}

/// Publication is exclusive, but readers of completed generations need not
/// wait for a native request or mount transition. Both access paths share the
/// resource owner's single published-snapshot cell.
#[derive(Clone)]
pub(super) struct SharedOverlayResource {
    pub(super) publication: Arc<tokio::sync::Mutex<Option<OverlayResourceLease>>>,
    latest: Arc<Mutex<Option<OverlaySnapshot>>>,
}

impl SharedOverlayResource {
    pub(super) fn new(resource: OverlayResourceLease) -> Self {
        Self {
            latest: resource.latest.clone(),
            publication: Arc::new(tokio::sync::Mutex::new(Some(resource))),
        }
    }

    pub(super) fn latest_snapshot(&self) -> Option<OverlaySnapshot> {
        self.latest.lock().clone()
    }

    #[cfg(test)]
    pub(super) fn release(self) -> io::Result<()> {
        Arc::try_unwrap(self.publication)
            .map_err(|_| {
                io::Error::other(
                    "build publication still owns the resource; cleanup is unconfirmed",
                )
            })?
            .into_inner()
            .map_or(Ok(()), OverlayResourceLease::release)
    }

    /// The caller has stopped exact writers, drained hosted work, and detached
    /// their mounts. Child snapshots still retain the immutable backing layers.
    pub(super) async fn retire(&self) -> io::Result<()> {
        let mut slot = self.publication.lock().await;
        if let Some(resource) = slot.as_ref() {
            if !matches!(resource.publication, PublicationState::Writable) {
                return Err(io::Error::other("workspace publication remains unsettled"));
            }
            if resource
                .storage
                .uncertain
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(io::Error::other("inherited mount custody is unconfirmed"));
            }
        }
        if let Some(mut resource) = slot.take() {
            resource.custody.settled = true;
            *resource.storage.state.lock() = OverlayResourceState::Reclaimable;
            resource.latest.lock().take();
            let OverlayResourceLease {
                storage,
                layers,
                custody,
                ..
            } = resource;
            drop(layers);
            drop(custody);
            if let Ok(mut storage) = Arc::try_unwrap(storage) {
                storage.release()?;
            }
        }
        Ok(())
    }
}

/// Only the publication owner can construct a snapshot of a frozen generation.
/// Clones retain every backing resource, independently of the actor's lifetime.
#[derive(Clone, Debug)]
pub(super) struct OverlaySnapshot {
    layers: Arc<[OverlayLayer]>,
}

#[derive(Clone, Debug)]
struct OverlayLayer {
    path: PathBuf,
    storage: Arc<OverlayStorage>,
}

#[derive(Debug)]
struct OverlayStorage {
    path: PathBuf,
    root: PathBuf,
    state: Mutex<OverlayResourceState>,
    uncertain: std::sync::atomic::AtomicBool,
}

#[derive(Debug)]
struct CustodyGuard {
    dependencies: Vec<Arc<OverlayStorage>>,
    exposed: bool,
    settled: bool,
}

impl Drop for CustodyGuard {
    fn drop(&mut self) {
        if self.exposed && !self.settled {
            for storage in &self.dependencies {
                storage
                    .uncertain
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
}

/// Offline lifecycle cleanup has proved that no namespace references this tree.
pub(super) fn remove_unmounted_storage(path: &Path) -> io::Result<()> {
    let mut storage = OverlayStorage {
        path: path.to_owned(),
        root: path
            .parent()
            .ok_or_else(|| io::Error::other("storage lacks parent"))?
            .to_owned(),
        state: Mutex::new(OverlayResourceState::RetainedUnconfirmed),
        uncertain: std::sync::atomic::AtomicBool::new(false),
    };
    storage.release()
}

#[derive(Debug, PartialEq, Eq)]
enum OverlayResourceState {
    Unsubmitted,
    RetainedUnconfirmed,
    Reclaimable,
    Released,
}

#[derive(Debug)]
enum PublicationState {
    Writable,
    Unconfirmed(Box<PendingRotation>),
}

#[derive(Debug)]
struct PendingRotation {
    recovery: OverlayRecovery,
    next: PathBuf,
    upper: PathBuf,
    work: PathBuf,
    frozen: Vec<OverlayLayer>,
}

impl OverlayResourceLease {
    pub(super) fn allocate_path(
        path: PathBuf,
        inherited: Option<OverlaySnapshot>,
    ) -> io::Result<Self> {
        // Exclusive creation is required even when the prior launch is uncertain.
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "overlay resource has no parent",
            )
        })?;
        let name = path
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "overlay resource has no directory name",
                )
            })?
            .to_owned();
        tidepool_atomic_write::create_dir_all_durable(parent)?;
        let parent = parent.canonicalize()?;
        let path = parent.join(name);
        std::fs::create_dir(&path)?;
        tidepool_atomic_write::sync_parent_directory(&path)?;
        let storage = Arc::new(OverlayStorage {
            root: parent.to_path_buf(),
            path,
            state: Mutex::new(OverlayResourceState::Unsubmitted),
            uncertain: std::sync::atomic::AtomicBool::new(false),
        });
        let latest = inherited.clone();
        let layers = match inherited {
            Some(snapshot) => snapshot.layers.to_vec(),
            None => {
                let base = storage.path.join("base");
                std::fs::create_dir(&base)?;
                vec![OverlayLayer {
                    path: base,
                    storage: storage.clone(),
                }]
            }
        };
        let upper = storage.path.join("upper");
        let work = storage.path.join("work");
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        let empty_upper = Some(SourceStamp::from(&std::fs::symlink_metadata(&upper)?));
        let custody = CustodyGuard {
            dependencies: std::iter::once(storage.clone())
                .chain(layers.iter().map(|layer| layer.storage.clone()))
                .collect(),
            exposed: false,
            settled: false,
        };
        Ok(Self {
            storage,
            layers,
            upper,
            work,
            latest: Arc::new(Mutex::new(latest)),
            publication: PublicationState::Writable,
            empty_upper,
            custody,
        })
    }

    pub(super) fn imported_base(&self) -> io::Result<(&Path, OverlaySnapshot)> {
        if self.layers.len() != 1 || self.layers[0].storage.path != self.storage.path {
            return Err(io::Error::other("source has no private imported base"));
        }
        Ok((
            &self.layers[0].path,
            OverlaySnapshot {
                layers: self.layers.clone().into(),
            },
        ))
    }

    /// Import ordinary host source into a private base before any mount exists.
    /// Exclusions are separately owned mount roots, never Git ignore patterns.
    pub(super) fn import_source(
        &self,
        source: &Path,
        excluded: &[&std::ffi::OsStr],
    ) -> io::Result<()> {
        if self.layers.len() != 1 || self.layers[0].storage.path != self.storage.path {
            return Err(io::Error::other(
                "source import requires a fresh private base",
            ));
        }
        let before = source_inventory(source, excluded)?;
        let mut entries = std::fs::read_dir(source)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.retain(|entry| !excluded.iter().any(|name| entry.file_name() == Some(*name)));
        entries.sort();
        if !entries.is_empty() {
            let output = std::process::Command::new("cp")
                .args(["--archive", "--reflink=auto", "--target-directory"])
                .arg(&self.layers[0].path)
                .arg("--")
                .args(entries)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "source import failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }
        if before != source_inventory(source, excluded)? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "source changed during import",
            ));
        }
        // Mount targets live in the immutable base so Bubblewrap does not
        // create them in the writable upper during view setup.
        std::fs::File::create_new(self.layers[0].path.join(".git"))?;
        std::fs::create_dir(self.layers[0].path.join(".shoal"))?;
        tidepool_node::copy_overlay_root_metadata(source, &self.layers[0].path)?;
        *self.latest.lock() = Some(OverlaySnapshot {
            layers: self.layers.clone().into(),
        });
        Ok(())
    }

    pub(super) fn prepare_root_metadata(&mut self) -> io::Result<()> {
        let inherited = self
            .layers
            .last()
            .ok_or_else(|| io::Error::other("source has no base"))?;
        tidepool_node::copy_overlay_root_metadata(&inherited.path, &self.upper)?;
        self.empty_upper = Some(SourceStamp::from(&std::fs::symlink_metadata(&self.upper)?));
        Ok(())
    }

    /// Bootstrap may update the upper directory's own timestamp while
    /// installing nested mounts. Capture the baseline before actor code runs.
    pub(super) fn record_bootstrap_upper(&mut self) -> io::Result<()> {
        self.empty_upper = if std::fs::read_dir(&self.upper)?.next().is_none() {
            Some(SourceStamp::from(&std::fs::symlink_metadata(&self.upper)?))
        } else {
            None
        };
        Ok(())
    }

    /// Reconcile only an already-started operation; never rotate a fresh view.
    pub(super) fn settle_pending(&mut self) -> io::Result<()> {
        match &self.publication {
            PublicationState::Writable => Ok(()),
            PublicationState::Unconfirmed(pending) => {
                let outcome = pending.recovery.reconcile();
                if let OverlayRotationOutcome::Unconfirmed(detail) = &outcome {
                    return Err(io::Error::other(detail.clone()));
                }
                self.settle_rotation(outcome).map(|_| ())
            }
        }
    }

    fn layers(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.layers.iter().map(|layer| layer.path.clone())
    }

    fn upper(&self) -> &Path {
        &self.upper
    }
    fn work(&self) -> &Path {
        &self.work
    }

    pub(super) fn mount(
        &self,
        mut boundary: ProcessMountBoundary,
        target: &Path,
    ) -> Result<ProcessMountBoundary, ProcessBoundaryError> {
        // Protect complete backing roots, including future sibling generations.
        for storage in
            std::iter::once(&self.storage).chain(self.layers.iter().map(|layer| &layer.storage))
        {
            boundary = boundary.with_read_only_overlay(&storage.root, &storage.root)?;
        }
        boundary.with_overlay_view(self.layers(), self.upper(), self.work(), target)
    }

    pub(super) fn latest_snapshot(&self) -> Option<OverlaySnapshot> {
        self.latest.lock().clone()
    }

    /// Called only inside native write admission. An empty upper with its exact
    /// post-publication root stamp cannot contain edits, whiteouts, or xattrs.
    pub(super) fn unchanged_snapshot(&self) -> io::Result<Option<OverlaySnapshot>> {
        if matches!(self.publication, PublicationState::Writable)
            && self.empty_upper.as_ref().is_some_and(|stamp| {
                std::fs::symlink_metadata(&self.upper)
                    .is_ok_and(|metadata| *stamp == SourceStamp::from(&metadata))
            })
            && std::fs::read_dir(&self.upper)?.next().is_none()
        {
            return Ok(self.latest_snapshot());
        }
        Ok(None)
    }

    pub(super) fn process_may_exist(&mut self) {
        // A lost host may drop in-memory leases while mounts survive. Mark
        // every dependency uncertain; explicit retirement or offline mount
        // proof is required before deletion.
        self.custody.exposed = true;
        *self.storage.state.lock() = OverlayResourceState::RetainedUnconfirmed;
    }

    /// Caller holds workspace admission across source and optional build rotation.
    pub(super) fn publish(
        &mut self,
        namespace: &MountNamespace,
        target: &Path,
        preserved_mounts: &[PathBuf],
    ) -> io::Result<OverlayRotationOutcome> {
        if let PublicationState::Unconfirmed(pending) = &self.publication {
            let outcome = pending.recovery.reconcile();
            return self.settle_rotation(outcome);
        }
        if *self.storage.state.lock() != OverlayResourceState::RetainedUnconfirmed {
            return Err(io::Error::other(
                "build publication requires retained process custody",
            ));
        }
        let generation = tempfile::Builder::new()
            .prefix("generation-")
            .tempdir_in(&self.storage.path)?;
        let next = generation.path();
        let upper = next.join("upper");
        let work = next.join("work");
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        tidepool_atomic_write::create_dir_all_durable(&next)?;
        let mut frozen = self.layers.clone();
        frozen.push(OverlayLayer {
            path: self.upper.clone(),
            storage: self.storage.clone(),
        });
        let rotation = OverlayRotation::prepare(
            target,
            &frozen
                .iter()
                .map(|layer| layer.path.clone())
                .collect::<Vec<_>>(),
            &upper,
            &work,
        )?
        .preserving_mounts(preserved_mounts)?;
        let prepared = namespace.prepare_overlay_rotation(rotation)?;
        // Once prepared, a lost receipt must never cause a second publication.
        // Preparation failures reclaim their unused directories. Once a mount
        // can exist, only confirmed transition settlement may release storage.
        let next = generation.keep();
        let (recovery, outcome) = prepared.apply();
        self.publication = PublicationState::Unconfirmed(Box::new(PendingRotation {
            recovery,
            next,
            upper,
            work,
            frozen,
        }));
        self.settle_rotation(outcome)
    }

    fn settle_rotation(
        &mut self,
        outcome: OverlayRotationOutcome,
    ) -> io::Result<OverlayRotationOutcome> {
        if matches!(outcome, OverlayRotationOutcome::Unconfirmed(_)) {
            return Ok(outcome);
        }
        let PublicationState::Unconfirmed(pending) =
            std::mem::replace(&mut self.publication, PublicationState::Writable)
        else {
            return Err(io::Error::other(
                "build rotation has no retained transition",
            ));
        };
        let PendingRotation {
            next,
            upper,
            work,
            frozen,
            ..
        } = *pending;
        match &outcome {
            OverlayRotationOutcome::Rotated => {
                self.upper = upper;
                self.work = work;
                self.layers = frozen;
                self.empty_upper =
                    Some(SourceStamp::from(&std::fs::symlink_metadata(&self.upper)?));
                let previous = self.latest.lock().replace(OverlaySnapshot {
                    layers: self.layers.clone().into(),
                });
                drop(previous);
            }
            OverlayRotationOutcome::Busy
            | OverlayRotationOutcome::RecoveredOriginal
            | OverlayRotationOutcome::Unchanged(_)
            | OverlayRotationOutcome::Restored(_) => {
                self.publication = PublicationState::Writable;
                match std::fs::remove_dir_all(&next) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            OverlayRotationOutcome::Unconfirmed(_) => {
                unreachable!("unconfirmed transition retained above")
            }
        }
        Ok(outcome)
    }

    #[cfg(test)]
    pub(super) fn release(mut self) -> io::Result<()> {
        if *self.storage.state.lock() == OverlayResourceState::RetainedUnconfirmed {
            return Err(io::Error::other(
                "overlay resource retained: exact process and hosted work cleanup is unconfirmed",
            ));
        }
        // Snapshot and descendant leases still own the directory. Last-owner
        // reclamation is an explicit offline decision if they outlive retirement.
        self.custody.settled = true;
        let Self {
            storage,
            layers,
            custody,
            ..
        } = self;
        drop(layers);
        drop(custody);
        match Arc::try_unwrap(storage) {
            Ok(mut storage) => storage.release(),
            Err(_) => Ok(()),
        }
    }
}

/// Detect changes made by external writers outside native/host admission.
/// Do not follow symlinks or let Git ignore rules omit project files.
pub(super) fn source_inventory(
    root: &Path,
    excluded: &[&std::ffi::OsStr],
) -> io::Result<std::collections::BTreeMap<PathBuf, SourceStamp>> {
    let mut inventory = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                if path == root && excluded.iter().any(|name| entry.file_name() == *name) {
                    continue;
                }
                pending.push(entry.path());
            }
        }
        inventory.insert(path, SourceStamp::from(&metadata));
    }
    Ok(inventory)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceStamp {
    device: u64,
    inode: u64,
    mode: u32,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceManifest(std::collections::BTreeMap<PathBuf, SourceEntry>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceEntry {
    mode: u32,
    uid: u32,
    gid: u32,
    bytes: u64,
    modified: (i64, i64),
    data: SourceData,
    xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    hardlink_anchor: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceData {
    File(blake3::Hash),
    Link(PathBuf),
    Other,
}

/// The import and a later source must agree on visible bytes and metadata.
/// Inode and ctime are deliberately absent: the private copy has new inodes.
pub(super) fn source_manifest(
    root: &Path,
    excluded: &[&std::ffi::OsStr],
) -> io::Result<SourceManifest> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let mut entries = std::collections::BTreeMap::new();
    let mut links: std::collections::BTreeMap<(u64, u64), Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        let relative = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_path_buf();
        let data = if metadata.is_file() {
            let mut file = std::fs::File::open(&path)?;
            let mut hash = blake3::Hasher::new();
            let mut chunk = [0u8; 65_536];
            loop {
                let count = file.read(&mut chunk)?;
                if count == 0 {
                    break;
                }
                hash.update(&chunk[..count]);
            }
            links
                .entry((metadata.dev(), metadata.ino()))
                .or_default()
                .push(relative.clone());
            SourceData::File(hash.finalize())
        } else if metadata.file_type().is_symlink() {
            SourceData::Link(std::fs::read_link(&path)?)
        } else {
            SourceData::Other
        };
        if SourceStamp::from(&metadata) != SourceStamp::from(&std::fs::symlink_metadata(&path)?) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "source changed while computing its manifest",
            ));
        }
        entries.insert(
            relative,
            SourceEntry {
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                bytes: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                data,
                xattrs: source_xattrs(&path)?,
                hardlink_anchor: None,
            },
        );
        if metadata.is_dir() {
            for child in std::fs::read_dir(&path)? {
                let child = child?;
                if path == root && excluded.iter().any(|name| child.file_name() == *name) {
                    continue;
                }
                pending.push(child.path());
            }
        }
    }
    for group in links.values_mut() {
        group.sort();
        let anchor = group[0].clone();
        for path in group {
            entries
                .get_mut(path)
                .expect("manifest link path inserted")
                .hardlink_anchor = Some(anchor.clone());
        }
    }
    Ok(SourceManifest(entries))
}

fn source_xattrs(path: &Path) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut names = vec![0u8; 65_536];
    let count = rustix::fs::llistxattr(path, names.as_mut_slice()).map_err(io::Error::from)?;
    let mut attributes = Vec::new();
    for name in names[..count]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(name).map_err(io::Error::other)?;
        let mut value = vec![0u8; 65_536];
        let count = rustix::fs::lgetxattr(path, name.as_c_str(), value.as_mut_slice())
            .map_err(io::Error::from)?;
        value.truncate(count);
        attributes.push((name.as_bytes().to_vec(), value));
    }
    attributes.sort();
    Ok(attributes)
}

impl From<&std::fs::Metadata> for SourceStamp {
    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

impl OverlayStorage {
    fn release(&mut self) -> io::Result<()> {
        *self.state.get_mut() = OverlayResourceState::RetainedUnconfirmed;
        // Detached OverlayFS work/work directories have mode 000. Only walk
        // this exclusively owned tree; never follow source symlinks.
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![self.path.clone()];
        while let Some(path) = pending.pop() {
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_dir() {
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
                )?;
                for entry in std::fs::read_dir(&path)? {
                    pending.push(entry?.path());
                }
            }
        }
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {
                *self.state.get_mut() = OverlayResourceState::Released;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                *self.state.get_mut() = OverlayResourceState::Released;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for OverlayStorage {
    fn drop(&mut self) {
        if *self.state.get_mut() != OverlayResourceState::Released {
            tracing::debug!(path = %self.path.display(), state = ?self.state.get_mut(), "overlay storage retained for explicit cleanup");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdout, Command, Stdio};
    use tidepool_node::ProcessInvocation;

    #[tokio::test]
    async fn retired_parent_layers_survive_children_then_are_reclaimed() {
        let directory = tempfile::tempdir().unwrap();
        let parent_path = directory.path().join("parent");
        let child_path = directory.path().join("child");
        let parent_project = directory.path().join("p");
        let child_project = directory.path().join("c");
        let mut parent = OverlayResourceLease::allocate_path(parent_path.clone(), None).unwrap();
        let (mut parent_worker, parent_view) = Worker::start(&mut parent, &parent_project);
        parent_worker.exchange("write");
        parent
            .publish(&parent_view, &parent_project.join("target"), &[])
            .unwrap();
        assert!(parent.unchanged_snapshot().unwrap().is_some());
        let mut child =
            OverlayResourceLease::allocate_path(child_path.clone(), parent.latest_snapshot())
                .unwrap();
        assert!(child.unchanged_snapshot().unwrap().is_some());
        let (child_worker, child_view) = Worker::start(&mut child, &child_project);
        let parent = SharedOverlayResource::new(parent);
        let child = SharedOverlayResource::new(child);
        drop(parent_worker);
        parent_view
            .detach_retired_tree(&parent_project.join("target"))
            .unwrap();
        parent.retire().await.unwrap();
        assert!(parent_path.exists(), "child retains inherited layers");
        assert!(child_view
            .host_command(&child_project, "/bin/sh".as_ref())
            .unwrap()
            .args(["-ec", "test -s target/value"])
            .status()
            .unwrap()
            .success());
        drop(child_worker);
        child_view
            .detach_retired_tree(&child_project.join("target"))
            .unwrap();
        child.retire().await.unwrap();
        assert!(!child_path.exists());
        assert!(
            parent_path.exists(),
            "drop never reclaims inherited storage"
        );
        remove_unmounted_storage(&parent_path).unwrap();
        assert!(!parent_path.exists());
        child.retire().await.unwrap();
    }

    #[test]
    fn root_metadata_changes_prevent_snapshot_reuse() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("p");
        let mut resource =
            OverlayResourceLease::allocate_path(directory.path().join("storage"), None).unwrap();
        let (mut worker, view) = Worker::start(&mut resource, &project);
        worker.exchange("write");
        resource
            .publish(&view, &project.join("target"), &[])
            .unwrap();
        assert!(resource.unchanged_snapshot().unwrap().is_some());
        std::fs::set_permissions(&resource.upper, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(resource.unchanged_snapshot().unwrap().is_none());
    }

    #[tokio::test]
    async fn lost_descendant_custody_does_not_authorize_parent_reclamation() {
        let directory = tempfile::tempdir().unwrap();
        let parent_path = directory.path().join("parent");
        let project = directory.path().join("project");
        let mut parent = OverlayResourceLease::allocate_path(parent_path.clone(), None).unwrap();
        let (mut worker, view) = Worker::start(&mut parent, &project);
        worker.exchange("write");
        parent.publish(&view, &project.join("target"), &[]).unwrap();
        let mut child = OverlayResourceLease::allocate_path(
            directory.path().join("child"),
            parent.latest_snapshot(),
        )
        .unwrap();
        child.process_may_exist();
        drop(child); // No exact cleanup receipt: the claim must survive the handle.
        drop(worker);
        view.detach_retired_tree(&project.join("target")).unwrap();
        let parent = SharedOverlayResource::new(parent);
        assert!(parent.retire().await.is_err());
        assert!(parent_path.exists());
    }

    struct Worker {
        child: Child,
        output: BufReader<ChildStdout>,
    }

    impl Worker {
        fn start(lease: &mut OverlayResourceLease, project: &Path) -> (Self, MountNamespace) {
            std::fs::create_dir_all(project.join("target")).unwrap();
            let boundary =
                ProcessMountBoundary::new(project, [project.into()], [project.into()]).unwrap();
            let boundary = lease.mount(boundary, &project.join("target")).unwrap();
            lease.process_may_exist();
            Self::start_in(boundary, project)
        }

        fn start_in(boundary: ProcessMountBoundary, project: &Path) -> (Self, MountNamespace) {
            let invocation = boundary.wrap(
                "bwrap",
                ProcessInvocation {
                    program: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        include_str!("overlay_resource/worker.sh").into(),
                        "build-worker".into(),
                        project.join("target").to_str().unwrap().into(),
                    ],
                },
            );
            let mut child = Command::new(invocation.program)
                .args(invocation.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let mut output = BufReader::new(child.stdout.take().unwrap());
            let mut pid = String::new();
            output.read_line(&mut pid).unwrap();
            let pid = pid.trim().parse().unwrap();
            let namespace = MountNamespace::capture(pid).unwrap();
            (Self { child, output }, namespace)
        }

        fn exchange(&mut self, command: &str) -> String {
            writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
            let mut line = String::new();
            self.output.read_line(&mut line).unwrap();
            line.trim().into()
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            drop(self.child.stdin.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn source_publication_preserves_the_independent_build_mount() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let project = root.join("project");
        let target = project.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let mut source =
            OverlayResourceLease::allocate_path(root.join("storage/source"), None).unwrap();
        std::fs::create_dir(source.layers[0].path.join("target")).unwrap();
        std::fs::write(source.layers[0].path.join("file"), "before").unwrap();
        let mut build =
            OverlayResourceLease::allocate_path(root.join("storage/build"), None).unwrap();
        let boundary =
            ProcessMountBoundary::new(&project, [project.clone()], [project.clone()]).unwrap();
        let boundary = source.mount(boundary, &project).unwrap();
        let boundary = build.mount(boundary, &target).unwrap();
        source.process_may_exist();
        build.process_may_exist();
        let (mut worker, namespace) = Worker::start_in(boundary, &project);
        let shell = |namespace: &MountNamespace, text: &str| {
            let output = namespace
                .host_command(&project, "/bin/sh".as_ref())
                .unwrap()
                .args(["-ec", text])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        shell(&namespace, "printf after > file");
        assert_eq!(worker.exchange("hold"), "held");
        let preserved = [target.clone()];
        assert!(matches!(
            source.publish(&namespace, &project, &preserved).unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        assert_eq!(worker.exchange("write"), "wrote");
        assert!(matches!(
            build.publish(&namespace, &target, &[]).unwrap(),
            OverlayRotationOutcome::Busy
        ));
        assert_eq!(shell(&namespace, "cat file"), b"after");
        assert!(build.latest_snapshot().is_none());

        assert!(matches!(
            source.publish(&namespace, &project, &preserved).unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        assert_eq!(worker.exchange("write"), "wrote");
        assert_eq!(
            std::fs::read_to_string(build.upper().join("value")).unwrap(),
            "2\n"
        );
        assert_eq!(worker.exchange("close"), "closed");

        let mut child_source = OverlayResourceLease::allocate_path(
            root.join("storage/child-source"),
            source.latest_snapshot(),
        )
        .unwrap();
        let boundary =
            ProcessMountBoundary::new(&project, [project.clone()], [project.clone()]).unwrap();
        let boundary = child_source.mount(boundary, &project).unwrap();
        child_source.process_may_exist();
        let child = boundary
            .prepare_view(
                "bwrap",
                std::time::Instant::now() + std::time::Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(
            shell(
                &child,
                "cat file; test ! -e target/value; printf child > file"
            ),
            b"after"
        );
        assert_eq!(shell(&namespace, "cat file"), b"after");
        assert_eq!(shell(&child, "cat file"), b"child");
    }

    #[test]
    fn allocation_retains_uncertain_process_and_refuses_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("build");
        let mut lease = OverlayResourceLease::allocate_path(path.clone(), None).unwrap();
        lease.process_may_exist();
        assert!(lease.release().is_err());
        assert!(path.exists());
        assert_eq!(
            OverlayResourceLease::allocate_path(path, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn prelaunch_drop_retains_and_explicit_release_reclaims_exclusive_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("build");
        drop(OverlayResourceLease::allocate_path(path.clone(), None).unwrap());
        assert!(path.exists());
        remove_unmounted_storage(&path).unwrap();
        assert!(!path.exists());
        let shared = SharedOverlayResource::new(
            OverlayResourceLease::allocate_path(path.clone(), None).unwrap(),
        );
        let pending_publication = shared.clone();
        assert!(shared.release().is_err());
        assert!(path.exists());
        pending_publication.release().unwrap();
        assert!(!path.exists());
        OverlayResourceLease::allocate_path(path.clone(), None)
            .unwrap()
            .release()
            .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn forty_generations_share_artifacts_and_preserve_whiteouts() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let mut parent =
            OverlayResourceLease::allocate_path(directory.path().join("storage/parent"), None)
                .unwrap();
        let artifact = parent.layers[0].path.join("artifact");
        std::fs::write(&artifact, vec![42u8; 1024 * 1024]).unwrap();
        std::fs::write(parent.layers[0].path.join("deleted"), "old").unwrap();
        let original = std::fs::metadata(&artifact).unwrap();
        let (mut worker, namespace) = Worker::start(&mut parent, &project);
        let output = namespace
            .host_command(&project, "/bin/sh".as_ref())
            .unwrap()
            .args(["-ec", "rm target/deleted; printf preserved > target/kept"])
            .output()
            .unwrap();
        assert!(output.status.success());
        for _ in 0..40 {
            assert_eq!(worker.exchange("write"), "wrote");
            assert!(matches!(
                parent
                    .publish(&namespace, &project.join("target"), &[])
                    .unwrap(),
                OverlayRotationOutcome::Rotated
            ));
        }
        assert_eq!(parent.layers.len(), 41);
        assert_eq!(std::fs::metadata(&artifact).unwrap().ino(), original.ino());
        assert_eq!(
            std::fs::metadata(&artifact).unwrap().blocks(),
            original.blocks()
        );
        assert_eq!(
            parent
                .layers
                .iter()
                .filter(|layer| layer.path.join("artifact").exists())
                .count(),
            1
        );
        let output = namespace.host_command(&project, "/bin/sh".as_ref()).unwrap()
            .args(["-ec", "test ! -e target/deleted; test \"$(cat target/kept)\" = preserved; test -s target/artifact"]).output().unwrap();
        assert!(output.status.success());
        assert_eq!(worker.exchange("hold"), "held");
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Busy
        ));
        assert_eq!(worker.exchange("close"), "closed");
    }

    #[test]
    fn warm_generation_survives_busy_parent_and_independent_child() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let project = root.join("project");
        let mut parent =
            OverlayResourceLease::allocate_path(root.join("storage/parent"), None).unwrap();
        let (mut worker, namespace) = Worker::start(&mut parent, &project);
        assert_eq!(worker.exchange("write"), "wrote");
        let outcome = parent
            .publish(&namespace, &project.join("target"), &[])
            .unwrap();
        assert!(
            matches!(outcome, OverlayRotationOutcome::Rotated),
            "{outcome:?}"
        );
        let snapshot = parent.latest_snapshot().unwrap();
        let warm_layer = snapshot.layers.last().unwrap().path.clone();
        assert_eq!(
            std::fs::read_to_string(warm_layer.join("value")).unwrap(),
            "1\n"
        );
        assert_eq!(worker.exchange("hold"), "held");
        let outcome = parent
            .publish(&namespace, &project.join("target"), &[])
            .unwrap();
        assert!(
            matches!(outcome, OverlayRotationOutcome::Busy),
            "{outcome:?}"
        );
        assert_eq!(
            parent
                .latest_snapshot()
                .unwrap()
                .layers
                .last()
                .unwrap()
                .path,
            warm_layer
        );
        assert_eq!(worker.exchange("close"), "closed");
        let layer_count = parent.layers.len();
        assert_eq!(
            parent
                .latest_snapshot()
                .unwrap()
                .layers
                .last()
                .unwrap()
                .path,
            warm_layer
        );
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        assert_eq!(parent.layers.len(), layer_count + 1);
        assert_eq!(
            parent.latest_snapshot().unwrap().layers.len(),
            layer_count + 1
        );
        let mut child =
            OverlayResourceLease::allocate_path(root.join("storage/child"), Some(snapshot))
                .unwrap();
        assert!(
            child.latest_snapshot().is_some(),
            "inherited warm state remains available to grandchildren"
        );
        let (mut child_worker, child_namespace) = Worker::start(&mut child, &project);
        let output = child_namespace
            .host_command(&project, "cat".as_ref())
            .unwrap()
            .arg(project.join("target/value"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"1\n");
        assert_eq!(worker.exchange("write"), "wrote");
        assert_eq!(child_worker.exchange("write"), "wrote");
        assert_eq!(
            std::fs::read_to_string(parent.upper().join("value")).unwrap(),
            "2\n"
        );
        assert_eq!(
            std::fs::read_to_string(child.upper().join("value")).unwrap(),
            "1\n"
        );
        assert_eq!(
            std::fs::read_to_string(warm_layer.join("value")).unwrap(),
            "1\n"
        );
        assert_eq!(worker.exchange("close"), "closed");
        assert_eq!(worker.exchange("quit"), "");
        assert!(worker.child.wait().unwrap().success());
        assert!(
            parent.release().is_err(),
            "worker exit alone is not full cleanup evidence"
        );
        assert!(warm_layer.exists());
        assert_eq!(child_worker.exchange("write"), "wrote");
        drop(child);
        assert!(
            warm_layer.exists(),
            "uncertain child custody must retain inherited storage"
        );
    }
}
