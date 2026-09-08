//! Durable sequencing joins native admission to an overlay resource publisher.

use super::*;
use std::num::NonZeroU64;
use tidepool_agent::interactive::{PublicationIdentity, PublicationOperation, PublicationReply};
use tidepool_agent::{InteractiveAgentBackend, QueueReadyThread};

/// May complete an earlier retained attempt. Source admission must correlate
/// the sequence with its fork checkpoint before selecting these working files.
#[derive(Debug)]
pub(crate) enum NativePublication {
    Published {
        sequence: NonZeroU64,
        snapshot: OverlaySnapshot,
    },
    Skipped(PublicationSkip),
}

#[derive(Debug)]
pub(crate) enum PublicationSkip {
    NativeBusy,
    NativeUnavailable(String),
    NoNewGeneration,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct PublicationTarget {
    path: PathBuf,
    preserved_mounts: Vec<PathBuf>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    version: u32,
    thread: String,
    sequence: NonZeroU64,
    phase: Phase,
    native: Option<NativeOwner>,
    view_before: Option<Vec<u8>>,
    target: PublicationTarget,
}

#[derive(serde::Serialize, serde::Deserialize)]
enum Phase {
    Begin,
    Publish,
    Finish,
    Complete,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NativeOwner {
    pid: u32,
    start_ticks: u64,
    mount_namespace_inode: u64,
    cgroup_path: PathBuf,
}

impl OverlayResourceLease {
    pub(crate) fn native_publication_needs_retry(&self) -> bool {
        self.native_retry
    }

    pub(crate) async fn publish_native(
        &mut self,
        backend: &dyn InteractiveAgentBackend,
        thread: &QueueReadyThread,
        target: &Path,
        preserved_mounts: &[PathBuf],
    ) -> io::Result<NativePublication> {
        self.native_retry = true;
        let result = self
            .drive_native_publication(backend, thread, target, preserved_mounts)
            .await;
        if result.is_ok() {
            self.native_retry = false;
        }
        result
    }

    async fn drive_native_publication(
        &mut self,
        backend: &dyn InteractiveAgentBackend,
        thread: &QueueReadyThread,
        target: &Path,
        preserved_mounts: &[PathBuf],
    ) -> io::Result<NativePublication> {
        let mut preserved_mounts = preserved_mounts.to_vec();
        preserved_mounts.sort();
        preserved_mounts.dedup();
        let requested = PublicationTarget {
            path: target.to_owned(),
            preserved_mounts,
        };
        let path = self.storage.path.join("native-publication.json");
        let mut record: Record = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Record {
                version: 3,
                thread: thread.id().0.clone(),
                sequence: NonZeroU64::MIN,
                phase: Phase::Begin,
                native: None,
                view_before: None,
                target: PublicationTarget {
                    path: requested.path.clone(),
                    preserved_mounts: requested.preserved_mounts.clone(),
                },
            },
            Err(error) => return Err(error),
        };
        if record.version != 3 || record.thread != thread.id().0 || record.target != requested {
            return Err(io::Error::other(
                "publication belongs to another native binding, target, or record version",
            ));
        }
        if matches!(record.phase, Phase::Publish | Phase::Finish)
            && (record.native.is_none() || record.view_before.is_none())
        {
            return Err(io::Error::other(
                "publication record is missing transition evidence",
            ));
        }
        if matches!(record.phase, Phase::Complete) {
            record.sequence = record
                .sequence
                .checked_add(1)
                .ok_or_else(|| io::Error::other("publication sequence exhausted"))?;
            record.phase = Phase::Begin;
            record.native = None;
            record.view_before = None;
        }
        // Persist before sending: a lost response must reuse this exact request.
        record.write(&path)?;
        if !matches!(record.phase, Phase::Finish) {
            let reply = backend
                .workspace_publication(
                    thread,
                    record.sequence,
                    PublicationOperation::Begin {
                        expected: record.native.as_ref().map(NativeOwner::identity),
                    },
                )
                .await
                .map_err(io::Error::other)?;
            let native = match reply {
                PublicationReply::Ready {
                    pid,
                    start_ticks,
                    mount_namespace_inode,
                    cgroup_path,
                } => NativeOwner {
                    pid,
                    start_ticks,
                    mount_namespace_inode,
                    cgroup_path,
                },
                PublicationReply::Busy if matches!(record.phase, Phase::Begin) => {
                    return Ok(NativePublication::Skipped(PublicationSkip::NativeBusy));
                }
                PublicationReply::Unavailable { detail: reason }
                    if matches!(record.phase, Phase::Begin) =>
                {
                    return Ok(NativePublication::Skipped(
                        PublicationSkip::NativeUnavailable(reason),
                    ));
                }
                PublicationReply::Settled if matches!(record.phase, Phase::Begin) => {
                    record.phase = Phase::Complete;
                    record.write(&path)?;
                    return Ok(NativePublication::Skipped(PublicationSkip::NoNewGeneration));
                }
                other => {
                    return Err(io::Error::other(format!(
                        "publication admission is inconsistent with retained state: {other:?}"
                    )))
                }
            };
            if record.native.as_ref().is_some_and(|prior| {
                prior.pid != native.pid
                    || prior.start_ticks != native.start_ticks
                    || prior.mount_namespace_inode != native.mount_namespace_inode
                    || prior.cgroup_path != native.cgroup_path
            }) {
                return Err(io::Error::other("publication native owner changed"));
            }
            let namespace = MountNamespace::capture_matching(
                native.pid,
                native.start_ticks,
                native.mount_namespace_inode,
            )?;
            record.native = Some(native);
            let view = std::fs::read(self.storage.path.join("view.json"))?;
            let already_recorded = matches!(record.phase, Phase::Publish)
                && record
                    .view_before
                    .as_ref()
                    .is_some_and(|before| *before != view);
            if already_recorded {
                // A durable view can precede pending-record cleanup and updating
                // the in-memory snapshot. Finish that work without rotating again.
                let recovered = if !matches!(self.publication, PublicationState::Writable) {
                    Some(self.publish(&namespace, target, &requested.preserved_mounts)?)
                } else {
                    self.reconcile_pending(&namespace, target, &requested.preserved_mounts)?
                };
                if recovered
                    .is_some_and(|outcome| !matches!(outcome, OverlayRotationOutcome::Rotated))
                {
                    return Err(io::Error::other(
                        "recorded native publication is not confirmed mounted",
                    ));
                }
            } else {
                if matches!(record.phase, Phase::Begin) {
                    record.view_before = Some(view);
                    record.phase = Phase::Publish;
                    record.write(&path)?;
                }
                match self.publish(&namespace, target, &requested.preserved_mounts)? {
                    OverlayRotationOutcome::Unconfirmed(detail) => {
                        return Err(io::Error::other(detail))
                    }
                    OverlayRotationOutcome::Rotated
                    | OverlayRotationOutcome::Busy
                    | OverlayRotationOutcome::RecoveredOriginal
                    | OverlayRotationOutcome::Unchanged(_)
                    | OverlayRotationOutcome::Restored(_) => {}
                }
            }
            record.phase = Phase::Finish;
            record.write(&path)?;
        }
        match backend
            .workspace_publication(
                thread,
                record.sequence,
                PublicationOperation::Finish {
                    expected: record
                        .native
                        .as_ref()
                        .ok_or_else(|| io::Error::other("missing native publication identity"))?
                        .identity(),
                },
            )
            .await
            .map_err(io::Error::other)?
        {
            PublicationReply::Settled => {
                let result = self.publication_result(&record)?;
                record.phase = Phase::Complete;
                record.write(&path)?;
                Ok(result)
            }
            other => Err(io::Error::other(format!(
                "publication completion is unconfirmed: {other:?}"
            ))),
        }
    }

    fn publication_result(&self, record: &Record) -> io::Result<NativePublication> {
        let bytes = std::fs::read(self.storage.path.join("view.json"))?;
        let view: ViewRecord = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let before: ViewRecord = serde_json::from_slice(
            record
                .view_before
                .as_deref()
                .ok_or_else(|| io::Error::other("publication has no starting view"))?,
        )
        .map_err(io::Error::other)?;
        if view.layers == before.layers {
            return Ok(NativePublication::Skipped(PublicationSkip::NoNewGeneration));
        }
        let snapshot = self
            .latest_snapshot()
            .ok_or_else(|| io::Error::other("published view has no retained snapshot"))?;
        if !view.warm
            || view.version != 1
            || view.layers
                != snapshot
                    .layers
                    .iter()
                    .map(|layer| layer.path.clone())
                    .collect::<Vec<_>>()
        {
            return Err(io::Error::other(
                "published view does not match its retained snapshot",
            ));
        }
        Ok(NativePublication::Published {
            sequence: record.sequence,
            snapshot,
        })
    }
}

impl Record {
    fn write(&self, path: &Path) -> io::Result<()> {
        Ok(tidepool_atomic_write::write_durable(
            path,
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )?)
    }
}

impl NativeOwner {
    fn identity(&self) -> PublicationIdentity {
        PublicationIdentity {
            pid: self.pid,
            start_ticks: self.start_ticks,
            mount_namespace_inode: self.mount_namespace_inode,
        }
    }
}
