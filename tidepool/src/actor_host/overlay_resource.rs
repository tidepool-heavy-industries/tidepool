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
    rotations: usize,
    consolidations: usize,
    covered_layers: Vec<OverlayLayer>,
}

/// Publication is exclusive, but readers of completed generations need not
/// wait for a native request or mount transition. Both access paths share the
/// resource owner's single published-snapshot cell.
#[derive(Clone)]
pub(super) struct SharedOverlayResource {
    pub(super) publication: Arc<tokio::sync::Mutex<OverlayResourceLease>>,
    latest: Arc<Mutex<Option<OverlaySnapshot>>>,
}

impl SharedOverlayResource {
    pub(super) fn new(resource: OverlayResourceLease) -> Self {
        Self {
            latest: resource.latest.clone(),
            publication: Arc::new(tokio::sync::Mutex::new(resource)),
        }
    }

    pub(super) fn latest_snapshot(&self) -> Option<OverlaySnapshot> {
        self.latest.lock().clone()
    }

    pub(super) fn release(self) -> io::Result<()> {
        Arc::try_unwrap(self.publication)
            .map_err(|_| {
                io::Error::other(
                    "build publication still owns the resource; cleanup is unconfirmed",
                )
            })?
            .into_inner()
            .release()
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
}

#[derive(Debug, PartialEq, Eq)]
enum OverlayResourceState {
    Unsubmitted,
    RetainedUnconfirmed,
    Released,
}

#[derive(Debug)]
enum PublicationState {
    Writable,
    NeedsRecord {
        bytes: Vec<u8>,
        snapshot: OverlaySnapshot,
    },
    Unconfirmed(Box<PendingRotation>),
}

#[derive(Debug)]
struct PendingRotation {
    recovery: OverlayRecovery,
    next: PathBuf,
    upper: PathBuf,
    work: PathBuf,
    frozen: Vec<OverlayLayer>,
    bytes: Vec<u8>,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct ViewRecord {
    version: u32,
    layers: Vec<PathBuf>,
    upper: PathBuf,
    work: PathBuf,
    warm: bool,
}

impl ViewRecord {
    fn new(layers: &[OverlayLayer], upper: &Path, work: &Path, warm: bool) -> Self {
        Self {
            version: 1,
            layers: layers.iter().map(|layer| layer.path.clone()).collect(),
            upper: upper.to_owned(),
            work: work.to_owned(),
            warm,
        }
    }
}

fn encode_view(
    layers: &[OverlayLayer],
    upper: &Path,
    work: &Path,
    warm: bool,
) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&ViewRecord::new(layers, upper, work, warm)).map_err(io::Error::other)
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
        // Persist dependency paths before any process can acquire these mounts.
        // This is a new resource-local format; existing unmanifested resources
        // remain retained and cannot be adopted by exclusive allocation.
        let bytes = encode_view(&layers, &upper, &work, latest.is_some())?;
        tidepool_atomic_write::write_durable(&storage.path.join("view.json"), &bytes)?;
        Ok(Self {
            storage,
            layers,
            upper,
            work,
            latest: Arc::new(Mutex::new(latest)),
            publication: PublicationState::Writable,
            rotations: 0,
            consolidations: 0,
            covered_layers: Vec::new(),
        })
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> &Path {
        &self.storage.path
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
        tidepool_node::copy_overlay_root_metadata(source, &self.layers[0].path)
    }

    pub(super) fn prepare_git_pointer(&self, git_file: &Path) -> io::Result<()> {
        std::fs::copy(git_file, self.upper.join(".git"))?;
        std::fs::create_dir_all(self.upper.join(super::ACTOR_BUILD_TARGET))?;
        let inherited = self
            .layers
            .last()
            .ok_or_else(|| io::Error::other("source has no base"))?;
        tidepool_node::copy_overlay_root_metadata(&inherited.path, &self.upper)
    }

    /// Reconcile only an already-started operation; never rotate a fresh view.
    pub(super) fn settle_pending(&mut self) -> io::Result<()> {
        match &self.publication {
            PublicationState::Writable => Ok(()),
            PublicationState::NeedsRecord { .. } => self.record_publication(),
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

    pub(super) fn process_may_exist(&mut self) {
        // A lost host drops its in-memory leases while mounted children may
        // survive. Preserve both this resource and its inherited dependencies.
        *self.storage.state.lock() = OverlayResourceState::RetainedUnconfirmed;
        for layer in &self.layers {
            *layer.storage.state.lock() = OverlayResourceState::RetainedUnconfirmed;
        }
    }

    /// Flatten only immutable lowers, while the parent's current upper remains
    /// writable. The caller runs this before taking native writer admission.
    pub(super) fn consolidate(&mut self) -> io::Result<()> {
        const COMPACT_AT: usize = 8;
        if self.consolidations >= 8
            || self.layers.len() < COMPACT_AT
            || !matches!(self.publication, PublicationState::Writable)
        {
            return Ok(());
        }
        self.consolidations += 1;
        let temporary = tempfile::Builder::new()
            .prefix("compact-")
            .tempdir_in(&self.storage.path)?;
        let base = temporary.path().join("base");
        let project = temporary.path().join("view");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        for path in [&base, &project, &upper, &work] {
            std::fs::create_dir(path)?;
        }
        let top = self
            .layers
            .last()
            .ok_or_else(|| io::Error::other("overlay has no base"))?;
        tidepool_node::copy_overlay_root_metadata(&top.path, &upper)
            .map_err(|error| io::Error::other(format!("compact metadata: {error}")))?;
        let boundary =
            ProcessMountBoundary::new(&project, [self.storage.root.clone()], [base.clone()])
                .and_then(|boundary| boundary.with_project_root(&project))
                .and_then(|boundary| {
                    boundary.with_overlay_view(
                        self.layers.iter().map(|layer| layer.path.clone()),
                        &upper,
                        &work,
                        &project,
                    )
                })
                .map_err(io::Error::other)?;
        // A lost bootstrap receipt leaves unknown mounts; preserve their backing
        // files. Successful copy commands and namespace teardown precede adoption.
        let temporary = temporary.keep();
        let view = boundary
            .with_read_only_project()
            .prepare_view(
                "bwrap",
                std::time::Instant::now() + super::PROCESS_OPERATION_TIMEOUT,
            )
            .map_err(|error| io::Error::other(format!("compact view: {error}")))?;
        let output = view
            .host_command(&project, "cp".as_ref())
            .map_err(|error| io::Error::other(format!("compact command: {error}")))?
            .args(["--archive", "--reflink=auto", "--"])
            .arg(project.join("."))
            .arg(&base)
            .output()
            .map_err(|error| io::Error::other(format!("compact copy: {error}")))?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "overlay consolidation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        drop(view);
        // OverlayFS creates its private work/work directory with mode 000.
        // The copy process and namespace are gone; this scratch is exclusively
        // ours and can now be made traversable for deletion.
        use std::os::unix::fs::PermissionsExt;
        let kernel_work = work.join("work");
        if kernel_work.is_dir() {
            std::fs::set_permissions(&kernel_work, std::fs::Permissions::from_mode(0o700))?;
        }
        for path in [&project, &upper, &work] {
            std::fs::remove_dir_all(path).map_err(|error| {
                io::Error::other(format!("compact cleanup {}: {error}", path.display()))
            })?;
        }
        let compact = OverlayLayer {
            path: base,
            storage: self.storage.clone(),
        };
        let bytes = encode_view(
            std::slice::from_ref(&compact),
            &self.upper,
            &self.work,
            true,
        )?;
        tidepool_atomic_write::write_durable(&self.storage.path.join("view.json"), &bytes)?;
        self.covered_layers
            .extend(std::mem::replace(&mut self.layers, vec![compact]));
        *self.latest.lock() = Some(OverlaySnapshot {
            layers: self.layers.clone().into(),
        });
        tracing::debug!(path = %temporary.display(), "consolidated immutable overlay layers");
        Ok(())
    }

    /// Caller holds workspace admission across source and optional build rotation.
    pub(super) fn publish(
        &mut self,
        namespace: &MountNamespace,
        target: &Path,
        preserved_mounts: &[PathBuf],
    ) -> io::Result<OverlayRotationOutcome> {
        if matches!(self.publication, PublicationState::NeedsRecord { .. }) {
            self.record_publication()?;
            return Ok(OverlayRotationOutcome::Rotated);
        }
        if let PublicationState::Unconfirmed(pending) = &self.publication {
            let outcome = pending.recovery.reconcile();
            return self.settle_rotation(outcome);
        }
        if *self.storage.state.lock() != OverlayResourceState::RetainedUnconfirmed {
            return Err(io::Error::other(
                "build publication requires retained process custody",
            ));
        }
        // Covered mounts remain live until native cwd refresh and retirement.
        // Bound them independently of compacted lower depth.
        const MAX_ROTATIONS: usize = 32;
        if self.rotations >= MAX_ROTATIONS || self.layers.len() >= MAX_ROTATIONS {
            return Ok(OverlayRotationOutcome::Unchanged(io::Error::other(
                "workspace snapshot capacity reached",
            )));
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
        let pending = encode_view(&frozen, &upper, &work, true)?;
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
            bytes: pending,
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
            bytes,
            ..
        } = *pending;
        match &outcome {
            OverlayRotationOutcome::Rotated => {
                // Mount state is known even if recording it subsequently fails.
                // Retry the record, never rotate the filesystem a second time.
                self.rotations += 1;
                self.upper = upper;
                self.work = work;
                self.layers = frozen;
                self.publication = PublicationState::NeedsRecord {
                    bytes,
                    snapshot: OverlaySnapshot {
                        layers: self.layers.clone().into(),
                    },
                };
                self.record_publication()?;
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

    fn record_publication(&mut self) -> io::Result<()> {
        let PublicationState::NeedsRecord { bytes, snapshot } = &self.publication else {
            return Err(io::Error::other(
                "build publication has no confirmed mount record",
            ));
        };
        tidepool_atomic_write::write_durable(&self.storage.path.join("view.json"), bytes)?;
        let previous = self.latest.lock().replace(snapshot.clone());
        drop(previous);
        self.publication = PublicationState::Writable;
        Ok(())
    }

    pub(super) fn release(self) -> io::Result<()> {
        if *self.storage.state.lock() == OverlayResourceState::RetainedUnconfirmed {
            return Err(io::Error::other(
                "overlay resource retained: exact process and hosted work cleanup is unconfirmed",
            ));
        }
        // Snapshot and descendant leases still own the directory. Last-owner
        // reclamation happens in OverlayStorage, never at actor retirement alone.
        let Self {
            storage, layers, ..
        } = self;
        drop(layers);
        match Arc::try_unwrap(storage) {
            Ok(mut storage) => storage.release(),
            Err(_) => Ok(()),
        }
    }
}

/// Detect changes made by external writers outside native/host admission.
/// Do not follow symlinks or let Git ignore rules omit project files.
fn source_inventory(
    root: &Path,
    excluded: &[&std::ffi::OsStr],
) -> io::Result<std::collections::BTreeMap<PathBuf, SourceStamp>> {
    use std::os::unix::fs::MetadataExt;
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
        inventory.insert(
            path,
            SourceStamp {
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                bytes: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            },
        );
    }
    Ok(inventory)
}

#[derive(PartialEq, Eq)]
struct SourceStamp {
    device: u64,
    inode: u64,
    mode: u32,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl OverlayStorage {
    fn release(&mut self) -> io::Result<()> {
        *self.state.get_mut() = OverlayResourceState::RetainedUnconfirmed;
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
        if *self.state.get_mut() == OverlayResourceState::Unsubmitted {
            let _ = self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdout, Command, Stdio};
    use tidepool_node::ProcessInvocation;

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

        // A failed manifest write retains the completed mount and nested views.
        std::fs::remove_file(source.path().join("view.json")).unwrap();
        std::fs::create_dir(source.path().join("view.json")).unwrap();
        assert!(source.publish(&namespace, &project, &preserved).is_err());
        std::fs::remove_dir(source.path().join("view.json")).unwrap();
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
    fn prelaunch_drop_and_explicit_release_reclaim_exclusive_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("build");
        drop(OverlayResourceLease::allocate_path(path.clone(), None).unwrap());
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
    fn consolidation_preserves_whiteouts_and_does_not_stop_a_live_writer() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let mut parent =
            OverlayResourceLease::allocate_path(directory.path().join("storage/parent"), None)
                .unwrap();
        std::fs::write(parent.layers[0].path.join("deleted"), "old").unwrap();
        let (mut worker, namespace) = Worker::start(&mut parent, &project);
        let output = namespace
            .host_command(&project, "/bin/sh".as_ref())
            .unwrap()
            .args(["-ec", "rm target/deleted; printf preserved > target/kept"])
            .output()
            .unwrap();
        assert!(output.status.success());
        for _ in 0..7 {
            assert_eq!(worker.exchange("write"), "wrote");
            assert!(matches!(
                parent
                    .publish(&namespace, &project.join("target"), &[])
                    .unwrap(),
                OverlayRotationOutcome::Rotated
            ));
        }
        assert_eq!(parent.layers.len(), 8);
        assert_eq!(worker.exchange("hold"), "held");
        let upper = parent.upper.clone();
        std::fs::remove_file(parent.path().join("view.json")).unwrap();
        std::fs::create_dir(parent.path().join("view.json")).unwrap();
        assert!(parent.consolidate().is_err());
        assert_eq!(parent.layers.len(), 8);
        assert_eq!(parent.upper, upper);
        assert_eq!(worker.exchange("write"), "wrote");
        std::fs::remove_dir(parent.path().join("view.json")).unwrap();
        parent.consolidate().unwrap();
        assert_eq!(parent.layers.len(), 1);
        assert_eq!(parent.upper, upper);
        assert_eq!(worker.exchange("write"), "wrote");
        assert!(!parent.layers[0].path.join("deleted").exists());
        assert_eq!(
            std::fs::read_to_string(parent.layers[0].path.join("kept")).unwrap(),
            "preserved"
        );
        assert_eq!(
            std::fs::read_to_string(parent.layers[0].path.join("value")).unwrap(),
            "7\n"
        );
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Busy
        ));
        assert_eq!(worker.exchange("close"), "closed");
        for _ in 7..32 {
            parent.consolidate().unwrap();
            assert!(matches!(
                parent
                    .publish(&namespace, &project.join("target"), &[])
                    .unwrap(),
                OverlayRotationOutcome::Rotated
            ));
        }
        let entries = std::fs::read_dir(parent.path()).unwrap().count();
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Unchanged(_)
        ));
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), entries);
        assert_eq!(worker.exchange("write"), "wrote");
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
        // A confirmed mount with a failed manifest write only needs its record
        // retried. The next attempt must not create another writable generation.
        assert_eq!(worker.exchange("close"), "closed");
        std::fs::remove_file(parent.path().join("view.json")).unwrap();
        std::fs::create_dir(parent.path().join("view.json")).unwrap();
        assert!(parent
            .publish(&namespace, &project.join("target"), &[])
            .is_err());
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
        std::fs::remove_dir(parent.path().join("view.json")).unwrap();
        let upper = parent.upper.clone();
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        assert_eq!(parent.layers.len(), layer_count);
        assert_eq!(parent.upper, upper);
        assert_eq!(parent.latest_snapshot().unwrap().layers.len(), layer_count);
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
