//! Shared overlay publication and storage custody for source and build views.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::ActorRef;
use tidepool_node::{
    MountNamespace, OverlayRecovery, OverlayRotation, OverlayRotationOutcome, ProcessBoundaryError,
    ProcessMountBoundary,
};

#[path = "overlay_resource/native_publication.rs"]
mod native_publication;
pub(super) use native_publication::{NativePublication, PublicationSkip};

#[derive(Debug)]
pub(super) struct OverlayResourceLease {
    storage: Arc<OverlayStorage>,
    layers: Vec<OverlayLayer>,
    upper: PathBuf,
    work: PathBuf,
    latest: Arc<Mutex<Option<OverlaySnapshot>>>,
    publication: PublicationState,
    native_retry: bool,
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

#[derive(serde::Serialize, serde::Deserialize)]
struct PendingViewRecord {
    version: u32,
    view: ViewRecord,
    recovery: tidepool_node::OverlayRecoveryRecord,
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
    pub(super) fn allocate_build(
        run_id: &str,
        actor: ActorRef,
        inherited: Option<OverlaySnapshot>,
    ) -> io::Result<Self> {
        let path = tidepool_runtime::paths::actor_build_resource_dir(
            run_id,
            actor.id.0,
            actor.incarnation.0,
        );
        Self::allocate_path(path, inherited)
    }

    fn allocate_path(path: PathBuf, inherited: Option<OverlaySnapshot>) -> io::Result<Self> {
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
            native_retry: false,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.storage.path
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

    /// Caller must hold native mutation admission and establish writer completion.
    /// Kept private to actor composition until that native handshake is connected.
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
        if let Some(outcome) = self.reconcile_pending(namespace, target, preserved_mounts)? {
            return Ok(outcome);
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
        let checkpoint = serde_json::to_vec(&PendingViewRecord {
            version: 2,
            view: ViewRecord::new(&frozen, &upper, &work, true),
            recovery: prepared.recovery_record()?,
        })
        .map_err(io::Error::other)?;
        tidepool_atomic_write::write_durable(&self.storage.path.join("pending.json"), &checkpoint)?;
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

    /// A possibly visible checkpoint write can survive even when the in-memory
    /// state never advanced. Reconcile that record before allocating a candidate.
    fn reconcile_pending(
        &mut self,
        namespace: &MountNamespace,
        target: &Path,
        preserved_mounts: &[PathBuf],
    ) -> io::Result<Option<OverlayRotationOutcome>> {
        let bytes = match std::fs::read(self.storage.path.join("pending.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let pending: PendingViewRecord =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if pending.version != 2 || pending.view.version != 1 || !pending.view.warm {
            return Err(io::Error::other(
                "unsupported pending overlay publication record",
            ));
        }
        let next = pending
            .view
            .upper
            .parent()
            .ok_or_else(|| io::Error::other("pending upper has no generation"))?
            .to_owned();
        if next.parent() != Some(self.storage.path.as_path())
            || pending.view.upper != next.join("upper")
            || pending.view.work != next.join("work")
        {
            return Err(io::Error::other(
                "pending overlay generation is outside its owning resource",
            ));
        }
        let current = ViewRecord::new(&self.layers, &self.upper, &self.work, true);
        let already_installed = current == pending.view;
        let mut frozen = self.layers.clone();
        if !already_installed {
            if self.upper.starts_with(&next)
                || self.work.starts_with(&next)
                || self
                    .layers
                    .iter()
                    .any(|layer| layer.path.starts_with(&next))
            {
                return Err(io::Error::other(
                    "pending overlay generation overlaps retained layers",
                ));
            }
            frozen.push(OverlayLayer {
                path: self.upper.clone(),
                storage: self.storage.clone(),
            });
        }
        if pending.view.layers
            != frozen
                .iter()
                .map(|layer| layer.path.clone())
                .collect::<Vec<_>>()
        {
            return Err(io::Error::other(
                "pending publication does not extend the retained overlay view",
            ));
        }
        let recovery = namespace.restore_overlay_recovery(pending.recovery)?;
        if !recovery.matches_replacement(
            target,
            &pending.view.layers,
            &pending.view.upper,
            &pending.view.work,
            preserved_mounts,
        )? {
            return Err(io::Error::other(
                "pending overlay view disagrees with its mount checkpoint",
            ));
        }
        let outcome = recovery.reconcile();
        if already_installed {
            if !matches!(outcome, OverlayRotationOutcome::Rotated) {
                return Err(io::Error::other(
                    "recorded overlay view is not confirmed mounted",
                ));
            }
            self.publication = PublicationState::NeedsRecord {
                bytes: serde_json::to_vec(&pending.view).map_err(io::Error::other)?,
                snapshot: OverlaySnapshot {
                    layers: self.layers.clone().into(),
                },
            };
            self.record_publication()?;
            return Ok(Some(outcome));
        }
        self.publication = PublicationState::Unconfirmed(Box::new(PendingRotation {
            recovery,
            next,
            upper: pending.view.upper.clone(),
            work: pending.view.work.clone(),
            frozen,
            bytes: serde_json::to_vec(&pending.view).map_err(io::Error::other)?,
        }));
        self.settle_rotation(outcome).map(Some)
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
                self.finish_publication()?;
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
        self.finish_publication()?;
        let previous = self.latest.lock().replace(snapshot.clone());
        drop(previous);
        self.publication = PublicationState::Writable;
        Ok(())
    }

    fn finish_publication(&self) -> io::Result<()> {
        let pending = self.storage.path.join("pending.json");
        match std::fs::remove_file(&pending) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        tidepool_atomic_write::sync_parent_directory(&pending)?;
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
        pid: u32,
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
            (Self { pid, child, output }, namespace)
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

        // Persisted retry must preserve the same nested mounts as the original
        // transition, even when its in-memory confirmation is unavailable.
        std::fs::remove_file(source.path().join("view.json")).unwrap();
        std::fs::create_dir(source.path().join("view.json")).unwrap();
        assert!(source.publish(&namespace, &project, &preserved).is_err());
        source.publication = PublicationState::Writable;
        std::fs::remove_dir(source.path().join("view.json")).unwrap();
        assert!(source.publish(&namespace, &project, &[]).is_err());
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

    #[tokio::test]
    async fn native_finish_retry_does_not_rotate_build_again() {
        use tidepool_agent::interactive::*;
        use tidepool_agent::{AgentBackendError, BackendThreadId};
        struct Backend {
            pid: u32,
            start_ticks: u64,
            mount_namespace_inode: u64,
            calls: Mutex<Vec<(u64, PublicationOperation)>>,
            begin_override: Mutex<Option<PublicationReply>>,
        }
        impl InteractiveAgentBackend for Backend {
            fn prepare_native_tool_policy(
                &self,
                _: InteractiveNativeToolPolicy,
                _: &Path,
            ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
                unreachable!()
            }
            fn render(
                &self,
                _: &InteractiveAgentSpec,
            ) -> Result<InteractiveAgentCommand, AgentBackendError> {
                unreachable!()
            }
            fn push<'a>(
                &'a self,
                _: &'a str,
                _: &'a QueueReadyThread,
                _: &'a str,
            ) -> InteractiveFuture<'a, ()> {
                unreachable!()
            }
            fn archive<'a>(
                &'a self,
                _: &'a str,
                _: &'a QueueReadyThread,
            ) -> InteractiveFuture<'a, ()> {
                unreachable!()
            }
            fn workspace_publication<'a>(
                &'a self,
                _: &'a QueueReadyThread,
                sequence: std::num::NonZeroU64,
                operation: PublicationOperation,
            ) -> InteractiveFuture<'a, PublicationReply> {
                Box::pin(async move {
                    let mut calls = self.calls.lock();
                    calls.push((sequence.get(), operation));
                    match operation {
                        PublicationOperation::Begin { .. } => {
                            Ok(self.begin_override.lock().take().unwrap_or(
                                PublicationReply::Ready {
                                    pid: self.pid,
                                    start_ticks: self.start_ticks,
                                    mount_namespace_inode: self.mount_namespace_inode,
                                    cgroup_path: "/test/writers".into(),
                                },
                            ))
                        }
                        PublicationOperation::Finish { .. } if calls.len() == 2 => {
                            Err(AgentBackendError::BackendUnavailable {
                                detail: "lost finish reply".into(),
                            })
                        }
                        PublicationOperation::Finish { .. } => Ok(PublicationReply::Settled),
                    }
                })
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let mut lease =
            OverlayResourceLease::allocate_path(directory.path().join("build"), None).unwrap();
        let (mut worker, namespace) = Worker::start(&mut lease, &project);
        use std::os::unix::fs::MetadataExt;
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", worker.pid)).unwrap();
        let start_ticks: u64 = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap();
        let mount_namespace_inode = std::fs::metadata(format!("/proc/{}/ns/mnt", worker.pid))
            .unwrap()
            .ino();
        assert!(MountNamespace::capture_matching(
            worker.pid,
            start_ticks + 1,
            mount_namespace_inode
        )
        .is_err());
        assert!(MountNamespace::capture_matching(
            worker.pid,
            start_ticks,
            mount_namespace_inode + 1
        )
        .is_err());
        let backend = Backend {
            pid: worker.pid,
            start_ticks,
            mount_namespace_inode,
            calls: Mutex::new(Vec::new()),
            begin_override: Mutex::new(None),
        };
        let binding = directory.path().join("binding.json");
        tidepool_agent::accept_interactive_session_binding(
            &binding,
            3,
            BackendThreadId(uuid::Uuid::new_v4().to_string()),
            None,
        )
        .await
        .unwrap();
        let thread = tidepool_agent::read_interactive_binding(&binding)
            .await
            .unwrap();
        assert!(lease
            .publish_native(&backend, &thread, &project.join("target"), &[])
            .await
            .is_err());
        assert!(lease.native_publication_needs_retry());
        let native_record = std::fs::read(lease.path().join("native-publication.json")).unwrap();
        let upper = lease.upper.clone();
        let layers = lease.layers.len();
        // A retained native sequence cannot be repurposed for another mount.
        assert!(lease
            .publish_native(&backend, &thread, &project, &[])
            .await
            .is_err());
        assert_eq!(backend.calls.lock().len(), 2);
        let result = lease
            .publish_native(&backend, &thread, &project.join("target"), &[])
            .await
            .unwrap();
        let NativePublication::Published { sequence, snapshot } = result else {
            panic!("the completed mount must return its published snapshot");
        };
        assert_eq!(sequence.get(), 1);
        assert_eq!(snapshot.layers.len(), layers);
        assert!(!lease.native_publication_needs_retry());
        assert_eq!(lease.upper, upper);
        assert_eq!(lease.layers.len(), layers);
        assert!(lease.latest_snapshot().is_some());
        assert!(matches!(
            backend.calls.lock().as_slice(),
            [
                (1, PublicationOperation::Begin { .. }),
                (1, PublicationOperation::Finish { .. }),
                (1, PublicationOperation::Finish { .. })
            ]
        ));
        assert_eq!(worker.exchange("hold"), "held");
        let result = lease
            .publish_native(&backend, &thread, &project.join("target"), &[])
            .await
            .unwrap();
        assert!(matches!(
            result,
            NativePublication::Skipped(PublicationSkip::NoNewGeneration)
        ));
        assert_eq!(lease.latest_snapshot().unwrap().layers.len(), layers);
        assert!(!lease.native_publication_needs_retry());
        assert_eq!(worker.exchange("close"), "closed");
        *backend.begin_override.lock() = Some(PublicationReply::Busy);
        assert!(matches!(
            lease
                .publish_native(&backend, &thread, &project.join("target"), &[])
                .await
                .unwrap(),
            NativePublication::Skipped(PublicationSkip::NativeBusy)
        ));
        *backend.begin_override.lock() = Some(PublicationReply::Unavailable {
            detail: "not local".into(),
        });
        let NativePublication::Skipped(PublicationSkip::NativeUnavailable(detail)) = lease
            .publish_native(&backend, &thread, &project.join("target"), &[])
            .await
            .unwrap()
        else {
            panic!("unavailable native admission cannot publish a snapshot");
        };
        assert_eq!(detail, "not local");
        assert_eq!(lease.latest_snapshot().unwrap().layers.len(), layers);
        assert!(matches!(
            &backend.calls.lock()[3..],
            [
                (2, PublicationOperation::Begin { .. }),
                (2, PublicationOperation::Finish { .. }),
                (3, PublicationOperation::Begin { .. }),
                (3, PublicationOperation::Begin { .. }),
            ]
        ));
        // Simulate interruption after the durable view write but before the
        // lease installed its latest snapshot and completed pending cleanup.
        let before = std::fs::read(lease.path().join("view.json")).unwrap();
        assert!(matches!(
            lease
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        let upper = lease.upper.clone();
        let snapshot = lease.latest.lock().take().unwrap();
        let layer_count = snapshot.layers.len();
        lease.publication = PublicationState::NeedsRecord {
            bytes: std::fs::read(lease.path().join("view.json")).unwrap(),
            snapshot,
        };
        let mut interrupted: serde_json::Value = serde_json::from_slice(&native_record).unwrap();
        interrupted["phase"] = "Publish".into();
        interrupted["sequence"] = 3.into();
        interrupted["view_before"] = serde_json::to_value(before).unwrap();
        std::fs::write(
            lease.path().join("native-publication.json"),
            serde_json::to_vec(&interrupted).unwrap(),
        )
        .unwrap();
        let NativePublication::Published { sequence, snapshot } = lease
            .publish_native(&backend, &thread, &project.join("target"), &[])
            .await
            .unwrap()
        else {
            panic!("record cleanup must recover the already-published generation");
        };
        assert_eq!(sequence.get(), 3);
        assert_eq!(snapshot.layers.len(), layer_count);
        assert_eq!(lease.upper, upper);
        assert!(!lease.native_publication_needs_retry());

        // Ordinary admission retains the selected warm generation in the owned
        // preparation, even when native execution is busy and the creator then
        // leaves the fleet before child bootstrap.
        use super::super::{
            custody_tests, ActorWorkspaceCustody, BuildInheritance, CreatorBuild, HostLaunchState,
            InteractiveApplicationOwner, NativeForkAdmission,
        };
        use tidepool_actor::{ForkWorkspaceAdmission, ForkWorkspaceSeed};
        let (_repo, _runtime, tree, _bindings, mut admission) = custody_tests::custody_fixture();
        let creator = ActorRef::first(tidepool_actor::ActorId(7));
        let creator_custody = admission
            .install_custody(creator, tree.id().as_str())
            .unwrap();
        let resource = SharedOverlayResource::new(
            OverlayResourceLease::allocate_path(
                directory.path().join("creator-cache"),
                Some(snapshot.clone()),
            )
            .unwrap(),
        );
        *backend.begin_override.lock() = Some(PublicationReply::Busy);
        let backend = Arc::new(backend);
        let before_calls = backend.calls.lock().len();
        let owners = Arc::new(Mutex::new(std::collections::HashMap::from([(
            creator,
            InteractiveApplicationOwner {
                creator_build: Some(CreatorBuild { resource, thread }),
                cancel: None,
                native_retirement: Default::default(),
                pane: Arc::new(Mutex::new(None)),
                fork_gate: None,
                custody: Some(creator_custody),
                scoped_retention: None,
                hosted: Arc::new(Mutex::new(None)),
                launch: HostLaunchState::Published,
                terminal: None,
                retirement: Arc::new(Mutex::new(None)),
            },
        )])));
        Arc::get_mut(&mut admission).unwrap().native = Some(NativeForkAdmission {
            owners: owners.clone(),
            backend: backend.clone(),
        });
        let denied = admission
            .admit(
                creator,
                "root/unauthorized".into(),
                ForkWorkspaceSeed::Explicit(tidepool_bridge_effects::WtWorktreeSpec {
                    spec_source: tidepool_bridge_effects::WtWorktreeSource::SourceCurrentRepository,
                    spec_label: "unauthorized".into(),
                    spec_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
                }),
                tidepool_actor::NativeToolClass::Coding,
            )
            .await;
        assert!(denied.is_err());
        assert_eq!(backend.calls.lock().len(), before_calls);
        let prepared = admission
            .admit(
                creator,
                "root/warm-child".into(),
                ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
                tidepool_actor::NativeToolClass::Coding,
            )
            .await
            .unwrap();
        assert_eq!(backend.calls.lock().len(), before_calls + 1);
        let inspection = admission
            .admit(
                creator,
                "root/inspection-child".into(),
                ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
                tidepool_actor::NativeToolClass::InspectionOnly,
            )
            .await
            .unwrap()
            .install(ActorRef::first(tidepool_actor::ActorId(9)))
            .unwrap();
        let inspection = (inspection.as_ref() as &dyn std::any::Any)
            .downcast_ref::<ActorWorkspaceCustody>()
            .unwrap();
        assert!(matches!(
            inspection.build_inheritance,
            BuildInheritance::Prepared(None)
        ));
        assert_eq!(backend.calls.lock().len(), before_calls + 1);
        let publishing = owners
            .lock()
            .get(&creator)
            .unwrap()
            .creator_build
            .as_ref()
            .unwrap()
            .resource
            .publication
            .clone();
        let publication_guard = publishing.lock().await;
        let contended = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            admission.admit(
                creator,
                "root/concurrent-child".into(),
                ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
                tidepool_actor::NativeToolClass::Coding,
            ),
        )
        .await
        .expect("fork admission must not wait for in-flight publication")
        .unwrap();
        assert_eq!(backend.calls.lock().len(), before_calls + 1);
        drop(publication_guard);
        let contended = contended
            .install(ActorRef::first(tidepool_actor::ActorId(10)))
            .unwrap();
        let contended = (contended.as_ref() as &dyn std::any::Any)
            .downcast_ref::<ActorWorkspaceCustody>()
            .unwrap();
        let BuildInheritance::Prepared(Some(previous)) = &contended.build_inheritance else {
            panic!("in-flight publication must leave the completed generation available");
        };
        assert!(Arc::ptr_eq(&previous.layers, &snapshot.layers));
        owners.lock().remove(&creator);
        let installed = prepared
            .install(ActorRef::first(tidepool_actor::ActorId(8)))
            .unwrap();
        let custody = (installed.as_ref() as &dyn std::any::Any)
            .downcast_ref::<ActorWorkspaceCustody>()
            .unwrap();
        let BuildInheritance::Prepared(Some(retained)) = &custody.build_inheritance else {
            panic!("busy creator must retain its completed warm snapshot for bootstrap");
        };
        assert!(Arc::ptr_eq(&retained.layers, &snapshot.layers));
        assert_eq!(backend.calls.lock().len(), before_calls + 1);
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
    fn warm_generation_survives_busy_parent_and_independent_child() {
        exercise_pending_recovery(false);
    }

    #[test]
    fn persisted_publication_recovers_before_owner_layout_advances() {
        exercise_pending_recovery(true);
    }

    fn exercise_pending_recovery(restore_previous_layout: bool) {
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
        assert!(!parent.path().join("pending.json").exists());
        // An unreadable checkpoint must prevent allocation and mount mutation.
        std::fs::create_dir(parent.path().join("pending.json")).unwrap();
        let before = std::fs::read_dir(parent.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(parent
            .publish(&namespace, &project.join("target"), &[])
            .is_err());
        assert_eq!(
            std::fs::read_dir(parent.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<std::collections::BTreeSet<_>>(),
            before
        );
        std::fs::remove_dir(parent.path().join("pending.json")).unwrap();
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Busy
        ));
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
        let old_layout = (
            parent.layers.clone(),
            parent.upper.clone(),
            parent.work.clone(),
        );
        std::fs::remove_file(parent.path().join("view.json")).unwrap();
        std::fs::create_dir(parent.path().join("view.json")).unwrap();
        assert!(parent
            .publish(&namespace, &project.join("target"), &[])
            .is_err());
        let mut checkpoint: serde_json::Value =
            serde_json::from_slice(&std::fs::read(parent.path().join("pending.json")).unwrap())
                .unwrap();
        assert_eq!(checkpoint["version"], 2);
        let recovered = namespace
            .restore_overlay_recovery(
                serde_json::from_value(checkpoint["recovery"].take()).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            recovered.reconcile(),
            OverlayRotationOutcome::Rotated
        ));
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
        // Lose the in-memory transition state while retaining the exact resource
        // and namespace owners. Retry must use the durable checkpoint alone.
        parent.publication = PublicationState::Writable;
        if restore_previous_layout {
            (parent.layers, parent.upper, parent.work) = old_layout;
        }
        let pending_path = parent.path().join("pending.json");
        let saved = std::fs::read(&pending_path).unwrap();
        let entries = || {
            std::fs::read_dir(root.join("storage/parent"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.is_dir())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let generations = entries();
        let mut invalid: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        invalid["version"] = 99.into();
        let invalid = serde_json::to_vec(&invalid).unwrap();
        std::fs::write(&pending_path, &invalid).unwrap();
        assert!(parent
            .publish(&namespace, &project.join("target"), &[])
            .is_err());
        assert_eq!(std::fs::read(&pending_path).unwrap(), invalid);
        assert_eq!(entries(), generations);
        std::fs::write(&pending_path, saved).unwrap();
        assert!(matches!(
            parent
                .publish(&namespace, &project.join("target"), &[])
                .unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        assert_eq!(parent.layers.len(), layer_count);
        assert_eq!(entries(), generations);
        assert!(!pending_path.exists());
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
