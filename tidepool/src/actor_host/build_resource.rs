//! Build storage custody for interactive actors.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::ActorRef;
use tidepool_node::{
    MountNamespace, OverlayRotation, OverlayRotationOutcome, ProcessBoundaryError,
    ProcessMountBoundary,
};

#[derive(Debug)]
pub(super) struct BuildResourceLease {
    storage: Arc<BuildStorage>,
    layers: Vec<BuildLayer>,
    upper: PathBuf,
    work: PathBuf,
    latest: Option<BuildSnapshot>,
    publication: PublicationState,
}

/// Only the publication owner can construct a snapshot of a frozen generation.
/// Clones retain every backing resource, independently of the actor's lifetime.
#[derive(Clone, Debug)]
pub(super) struct BuildSnapshot {
    layers: Arc<[BuildLayer]>,
}

#[derive(Clone, Debug)]
struct BuildLayer {
    path: PathBuf,
    storage: Arc<BuildStorage>,
}

#[derive(Debug)]
struct BuildStorage {
    path: PathBuf,
    root: PathBuf,
    state: Mutex<BuildResourceState>,
}

#[derive(Debug, PartialEq, Eq)]
enum BuildResourceState {
    Unsubmitted,
    RetainedUnconfirmed,
    Released,
}

#[derive(Debug)]
enum PublicationState {
    Writable,
    NeedsRecord {
        bytes: Vec<u8>,
        snapshot: BuildSnapshot,
    },
    Unconfirmed,
}

#[derive(serde::Serialize)]
struct ViewRecord<'a> {
    version: u32,
    layers: Vec<&'a Path>,
    upper: &'a Path,
    work: &'a Path,
    warm: bool,
}

fn encode_view(
    layers: &[BuildLayer],
    upper: &Path,
    work: &Path,
    warm: bool,
) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&ViewRecord {
        version: 1,
        layers: layers.iter().map(|layer| layer.path.as_path()).collect(),
        upper,
        work,
        warm,
    })
    .map_err(io::Error::other)
}

impl BuildResourceLease {
    pub(super) fn allocate(
        run_id: &str,
        actor: ActorRef,
        inherited: Option<BuildSnapshot>,
    ) -> io::Result<Self> {
        let path = tidepool_runtime::paths::actor_build_resource_dir(
            run_id,
            actor.id.0,
            actor.incarnation.0,
        );
        Self::allocate_path(path, inherited)
    }

    fn allocate_path(path: PathBuf, inherited: Option<BuildSnapshot>) -> io::Result<Self> {
        // Exclusive creation is required even when the prior launch is uncertain.
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "build resource has no parent")
        })?;
        tidepool_atomic_write::create_dir_all_durable(parent)?;
        std::fs::create_dir(&path)?;
        tidepool_atomic_write::sync_parent_directory(&path)?;
        let storage = Arc::new(BuildStorage {
            root: parent.to_path_buf(),
            path,
            state: Mutex::new(BuildResourceState::Unsubmitted),
        });
        let latest = inherited.clone();
        let layers = match inherited {
            Some(snapshot) => snapshot.layers.to_vec(),
            None => {
                let base = storage.path.join("base");
                std::fs::create_dir(&base)?;
                vec![BuildLayer {
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
            latest,
            publication: PublicationState::Writable,
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

    pub(super) fn latest_snapshot(&self) -> Option<BuildSnapshot> {
        self.latest.clone()
    }

    pub(super) fn process_may_exist(&mut self) {
        // A lost host drops its in-memory leases while mounted children may
        // survive. Preserve both this resource and its inherited dependencies.
        *self.storage.state.lock() = BuildResourceState::RetainedUnconfirmed;
        for layer in &self.layers {
            *layer.storage.state.lock() = BuildResourceState::RetainedUnconfirmed;
        }
    }

    /// Caller must hold native mutation admission and establish writer completion.
    /// Kept private to actor composition until that native handshake is connected.
    #[allow(dead_code)]
    pub(super) fn publish(
        &mut self,
        namespace: &MountNamespace,
        target: &Path,
    ) -> io::Result<OverlayRotationOutcome> {
        if matches!(self.publication, PublicationState::NeedsRecord { .. }) {
            self.record_publication()?;
            return Ok(OverlayRotationOutcome::Rotated);
        }
        if matches!(self.publication, PublicationState::Unconfirmed) {
            return Err(io::Error::other(
                "build publication requires reconciliation",
            ));
        }
        if *self.storage.state.lock() != BuildResourceState::RetainedUnconfirmed {
            return Err(io::Error::other(
                "build publication requires retained process custody",
            ));
        }
        let next = self
            .storage
            .path
            .join(format!("generation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&next)?;
        let upper = next.join("upper");
        let work = next.join("work");
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        tidepool_atomic_write::create_dir_all_durable(&next)?;
        let mut frozen = self.layers.clone();
        frozen.push(BuildLayer {
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
        )?;
        // Once prepared, a lost receipt must never cause a second publication.
        let pending = encode_view(&frozen, &upper, &work, true)?;
        tidepool_atomic_write::write_durable(&self.storage.path.join("pending.json"), &pending)?;
        self.publication = PublicationState::Unconfirmed;
        let outcome = namespace.rotate_overlay(rotation);
        match &outcome {
            OverlayRotationOutcome::Rotated => {
                // Mount state is known even if recording it subsequently fails.
                // Retry the record, never rotate the filesystem a second time.
                self.upper = upper;
                self.work = work;
                self.layers = frozen;
                self.publication = PublicationState::NeedsRecord {
                    bytes: pending,
                    snapshot: BuildSnapshot {
                        layers: self.layers.clone().into(),
                    },
                };
                self.record_publication()?;
            }
            OverlayRotationOutcome::Busy
            | OverlayRotationOutcome::Unchanged(_)
            | OverlayRotationOutcome::Restored(_) => {
                self.publication = PublicationState::Writable;
                std::fs::remove_dir_all(&next)?;
                self.finish_publication()?;
            }
            OverlayRotationOutcome::Unconfirmed(_) => {}
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
        self.latest = Some(snapshot.clone());
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
        if *self.storage.state.lock() == BuildResourceState::RetainedUnconfirmed {
            return Err(io::Error::other(
                "build resource retained: exact process and hosted work cleanup is unconfirmed",
            ));
        }
        // Snapshot and descendant leases still own the directory. Last-owner
        // reclamation happens in BuildStorage, never at actor retirement alone.
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

impl BuildStorage {
    fn release(&mut self) -> io::Result<()> {
        *self.state.get_mut() = BuildResourceState::RetainedUnconfirmed;
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {
                *self.state.get_mut() = BuildResourceState::Released;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                *self.state.get_mut() = BuildResourceState::Released;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for BuildStorage {
    fn drop(&mut self) {
        if *self.state.get_mut() == BuildResourceState::Unsubmitted {
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
        fn start(lease: &mut BuildResourceLease, project: &Path) -> (Self, MountNamespace) {
            std::fs::create_dir_all(project.join("target")).unwrap();
            let boundary =
                ProcessMountBoundary::new(project, [project.into()], [project.into()]).unwrap();
            let boundary = lease.mount(boundary, &project.join("target")).unwrap();
            let invocation = boundary.wrap(
                "bwrap",
                ProcessInvocation {
                    program: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        include_str!("build_resource/worker.sh").into(),
                        "build-worker".into(),
                        project.join("target").to_str().unwrap().into(),
                    ],
                },
            );
            lease.process_may_exist();
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
            let namespace = MountNamespace::capture(pid.trim().parse().unwrap()).unwrap();
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
    fn allocation_retains_uncertain_process_and_refuses_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("build");
        let mut lease = BuildResourceLease::allocate_path(path.clone(), None).unwrap();
        lease.process_may_exist();
        assert!(lease.release().is_err());
        assert!(path.exists());
        assert_eq!(
            BuildResourceLease::allocate_path(path, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn prelaunch_drop_and_explicit_release_reclaim_exclusive_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("build");
        drop(BuildResourceLease::allocate_path(path.clone(), None).unwrap());
        assert!(!path.exists());
        BuildResourceLease::allocate_path(path.clone(), None)
            .unwrap()
            .release()
            .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn warm_generation_survives_busy_parent_and_independent_child() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let project = root.join("project");
        let mut parent =
            BuildResourceLease::allocate_path(root.join("storage/parent"), None).unwrap();
        let (mut worker, namespace) = Worker::start(&mut parent, &project);
        assert_eq!(worker.exchange("write"), "wrote");
        let outcome = parent.publish(&namespace, &project.join("target")).unwrap();
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
        let outcome = parent.publish(&namespace, &project.join("target")).unwrap();
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
        // Preparation failed before invoking the mount owner. The old view is
        // known to be writable and retry needs no reconciliation.
        std::fs::create_dir(parent.path().join("pending.json")).unwrap();
        assert!(parent.publish(&namespace, &project.join("target")).is_err());
        std::fs::remove_dir(parent.path().join("pending.json")).unwrap();
        assert!(matches!(
            parent.publish(&namespace, &project.join("target")).unwrap(),
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
        std::fs::remove_file(parent.path().join("view.json")).unwrap();
        std::fs::create_dir(parent.path().join("view.json")).unwrap();
        assert!(parent.publish(&namespace, &project.join("target")).is_err());
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
        assert!(matches!(
            parent.publish(&namespace, &project.join("target")).unwrap(),
            OverlayRotationOutcome::Rotated
        ));
        assert_eq!(parent.layers.len(), layer_count);
        let mut child =
            BuildResourceLease::allocate_path(root.join("storage/child"), Some(snapshot)).unwrap();
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
