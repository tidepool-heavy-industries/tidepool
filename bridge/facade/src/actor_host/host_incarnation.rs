//! Durable epoch allocation for exact local actor identities.
//!
//! Ractor process IDs are local to one daemon process and may be reused after
//! restart. This owner claims one monotonically increasing incarnation while
//! holding a lifetime file lock. Every actor spawned by that daemon shares the
//! epoch, so an old `(ActorId, Incarnation)` can never authorize a new process.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exomonad_actor::Incarnation;
use serde::{Deserialize, Serialize};

const STATE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IncarnationState {
    version: u32,
    last_incarnation: u64,
}

/// Exclusive claim on one Exomonad host incarnation.
#[derive(Debug)]
pub(crate) struct HostIncarnationLease {
    incarnation: Incarnation,
    run_root: PathBuf,
    owner_lock: Option<HostRunLock>,
    released: tokio::sync::watch::Sender<HostLeaseRelease>,
}

/// Observation of the physical guard's last-owner release, without retaining it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostLeaseRelease {
    Pending,
    Released,
    Unconfirmed(String),
}

/// Shared host/maintenance exclusion; maintenance never allocates an incarnation.
#[derive(Debug)]
pub(super) struct HostRunLock(Option<File>);
impl HostRunLock {
    pub(super) fn existing(run_root: &Path) -> io::Result<Self> {
        Self::claim(run_root, false)
    }

    fn claim(run_root: &Path, create: bool) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(create)
            .truncate(false)
            .write(true)
            .open(run_root.join("host-incarnation.owner.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Self(Some(file))),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "another Exomonad host or maintenance operation already owns {}",
                    run_root.display()
                ),
            )),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    fn release(&mut self) -> io::Result<()> {
        let Some(file) = self.0.take() else {
            return Ok(());
        };
        // A fork can inherit this open description. Explicit unlock precedes
        // closing our file, and both precede any release acknowledgment.
        let result = file.unlock();
        drop(file);
        result
    }
}
impl Drop for HostRunLock {
    fn drop(&mut self) {
        // Concurrent fork may temporarily inherit the open description.
        // Unlock ends ownership without waiting for that child to exec.
        if let Err(error) = self.release() {
            tracing::warn!(%error, "Exomonad host owner unlock failed");
        }
    }
}

impl Drop for HostIncarnationLease {
    fn drop(&mut self) {
        let outcome = match self.owner_lock.take() {
            Some(mut lock) => {
                let result = lock.release();
                drop(lock);
                match result {
                    Ok(()) => HostLeaseRelease::Released,
                    Err(error) => HostLeaseRelease::Unconfirmed(error.to_string()),
                }
            }
            None => HostLeaseRelease::Unconfirmed("host owner guard was absent".into()),
        };
        self.released.send_replace(outcome);
    }
}

/// Issued once by consuming the exclusive lease before sharing it with services.
pub(super) struct HostBootRegistration(Arc<HostIncarnationLease>);

impl exomonad_actor::RecoveryBootRegistration for HostBootRegistration {
    fn into_run(self: Box<Self>) -> Arc<dyn tidepool_runtime::session::RecoveryRunAuthority> {
        self.0
    }
}

impl tidepool_runtime::session::RecoveryRunAuthority for HostIncarnationLease {
    fn owns_run(&self, run_root: &Path) -> io::Result<bool> {
        HostIncarnationLease::owns_run(self, run_root)
    }
}

impl HostIncarnationLease {
    /// Read the epoch owner's durable issuance fact without consulting run status.
    pub(crate) fn last_issued(run_root: &Path) -> io::Result<Option<Incarnation>> {
        read_previous(&run_root.join("host-incarnation.json"))
    }

    /// Observe physical exclusion without allocating a new host epoch.
    pub(crate) fn verify_released(run_root: &Path) -> io::Result<()> {
        match HostRunLock::existing(run_root) {
            Ok(lock) => {
                drop(lock);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::symlink_metadata(run_root.join("host-incarnation.json")) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(inaccessible) => Err(inaccessible),
                    Ok(_) => Err(io::Error::other("issued host epoch lost its owner lock")),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn into_boot_registration(self) -> (Arc<Self>, HostBootRegistration) {
        let lease = Arc::new(self);
        let registration = HostBootRegistration(Arc::clone(&lease));
        (lease, registration)
    }

    pub(crate) fn claim(directory: &tidepool_atomic_write::DirectoryAnchor) -> io::Result<Self> {
        directory.create_dir_all("")?;
        let run_root = directory.path();
        let owner_lock = HostRunLock::claim(run_root, true)?;

        let state_path = run_root.join("host-incarnation.json");
        let previous = read_previous(&state_path)?.map_or(0, |incarnation| incarnation.0);
        let next = previous.checked_add(1).ok_or_else(|| {
            io::Error::other(format!(
                "Exomonad host incarnation exhausted at {}",
                state_path.display()
            ))
        })?;
        let bytes = serde_json::to_vec_pretty(&IncarnationState {
            version: STATE_VERSION,
            last_incarnation: next,
        })
        .map_err(io::Error::other)?;
        tidepool_atomic_write::write_durable(&state_path, &bytes)
            .map_err(|error| io::Error::other(error.to_string()))?;

        Ok(Self {
            incarnation: Incarnation(next),
            run_root: fs::canonicalize(run_root)?,
            owner_lock: Some(owner_lock),
            released: tokio::sync::watch::channel(HostLeaseRelease::Pending).0,
        })
    }

    pub(crate) fn release_observation(&self) -> tokio::sync::watch::Receiver<HostLeaseRelease> {
        self.released.subscribe()
    }

    pub(crate) const fn incarnation(&self) -> Incarnation {
        self.incarnation
    }

    pub(crate) fn owns_run(&self, run_root: &Path) -> io::Result<bool> {
        Ok(self.run_root == fs::canonicalize(run_root)?)
    }
}

fn read_previous(path: &Path) -> io::Result<Option<Incarnation>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let state: IncarnationState = serde_json::from_slice(&bytes)
        .map_err(|error| invalid_state(path, format!("could not decode durable state: {error}")))?;
    if state.version != STATE_VERSION {
        return Err(invalid_state(
            path,
            format!(
                "unsupported durable state version {}; expected {STATE_VERSION}",
                state.version
            ),
        ));
    }
    if state.last_incarnation == 0 {
        return Err(invalid_state(
            path,
            "issued host incarnation must be nonzero",
        ));
    }
    Ok(Some(Incarnation(state.last_incarnation)))
}

fn invalid_state(path: &Path, detail: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "invalid Exomonad host incarnation state {}: {detail}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_receipt_waits_for_last_real_owner_and_closed_file() {
        let runtime = tempfile::tempdir().unwrap();
        let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap();
        let lease = HostIncarnationLease::claim(&anchor).unwrap();
        let receipt = lease.release_observation();
        let (lease, registration) = lease.into_boot_registration();
        let retained = lease.clone();
        let competing = OpenOptions::new()
            .write(true)
            .open(runtime.path().join("host-incarnation.owner.lock"))
            .unwrap();
        drop(lease);
        drop(registration);
        assert_eq!(*receipt.borrow(), HostLeaseRelease::Pending);
        assert!(matches!(
            competing.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(retained);
        // A late observer sees release only after the actual guard has closed.
        assert_eq!(*receipt.borrow(), HostLeaseRelease::Released);
        competing.try_lock().unwrap();
    }

    #[tokio::test]
    async fn cancelled_release_observer_does_not_release_real_custody() {
        let runtime = tempfile::tempdir().unwrap();
        let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap();
        let lease = Arc::new(HostIncarnationLease::claim(&anchor).unwrap());
        let receipt = lease.release_observation();
        let mut observing = receipt.clone();
        // Poll the real observation pending, then cancel only that waiter.
        tokio::select! {
            biased;
            result = observing.changed() => panic!("live guard unexpectedly settled: {result:?}"),
            () = std::future::ready(()) => {}
        }
        drop(observing);
        let competing = OpenOptions::new()
            .write(true)
            .open(runtime.path().join("host-incarnation.owner.lock"))
            .unwrap();
        assert_eq!(*receipt.borrow(), HostLeaseRelease::Pending);
        assert!(matches!(
            competing.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(lease);
        assert_eq!(*receipt.borrow(), HostLeaseRelease::Released);
        competing.try_lock().unwrap();
    }

    #[test]
    fn last_issued_preserves_absence_and_corruption() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            HostIncarnationLease::last_issued(root.path()).unwrap(),
            None
        );
        let directory = tidepool_atomic_write::DirectoryAnchor::open_existing(root.path()).unwrap();
        drop(HostIncarnationLease::claim(&directory).unwrap());
        assert_eq!(
            HostIncarnationLease::last_issued(root.path()).unwrap(),
            Some(Incarnation(1))
        );
        fs::write(root.path().join("host-incarnation.json"), b"not-json").unwrap();
        assert!(HostIncarnationLease::last_issued(root.path()).is_err());
    }

    #[test]
    fn release_probe_distinguishes_never_started_from_missing_issued_lock() {
        let root = tempfile::tempdir().unwrap();
        HostIncarnationLease::verify_released(root.path()).unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let directory = tidepool_atomic_write::DirectoryAnchor::open_existing(root.path()).unwrap();
        drop(HostIncarnationLease::claim(&directory).unwrap());
        fs::remove_file(root.path().join("host-incarnation.owner.lock")).unwrap();
        assert!(HostIncarnationLease::verify_released(root.path()).is_err());
        assert!(!root.path().join("host-incarnation.owner.lock").exists());
    }

    #[test]
    fn release_probe_does_not_allocate_an_epoch() {
        let root = tempfile::tempdir().unwrap();
        let directory = tidepool_atomic_write::DirectoryAnchor::open_existing(root.path()).unwrap();
        let first = HostIncarnationLease::claim(&directory).unwrap();
        assert_eq!(
            HostIncarnationLease::verify_released(root.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        HostIncarnationLease::verify_released(root.path()).unwrap();
        HostIncarnationLease::verify_released(root.path()).unwrap();
        assert_eq!(
            HostIncarnationLease::claim(&directory)
                .unwrap()
                .incarnation(),
            Incarnation(2)
        );
    }

    #[test]
    fn claims_monotonically_increasing_incarnations() {
        let runtime = tempfile::tempdir().unwrap();
        let first = HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap(),
        )
        .unwrap();
        assert_eq!(first.incarnation(), Incarnation(1));
        drop(first);

        let second = HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap(),
        )
        .unwrap();
        assert_eq!(second.incarnation(), Incarnation(2));
    }

    #[test]
    fn refuses_a_second_live_owner() {
        let runtime = tempfile::tempdir().unwrap();
        let _first = HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap(),
        )
        .unwrap();
        let error = HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("already owns"));
    }

    #[test]
    fn run_ownership_cannot_authorize_another_run() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let owner = HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(first.path()).unwrap(),
        )
        .unwrap();
        assert!(owner.owns_run(first.path()).unwrap());
        assert!(!owner.owns_run(second.path()).unwrap());
    }

    #[test]
    fn boot_registration_retains_exclusive_run_until_authority_released() {
        use exomonad_actor::RecoveryBootRegistration;

        let runtime = tempfile::tempdir().unwrap();
        let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap();
        let (lease, registration) = HostIncarnationLease::claim(&anchor)
            .unwrap()
            .into_boot_registration();
        drop(lease);
        assert!(HostIncarnationLease::claim(&anchor).is_err());
        let authority = Box::new(registration).into_run();
        assert!(authority.owns_run(runtime.path()).unwrap());
        assert!(HostIncarnationLease::claim(&anchor).is_err());
        drop(authority);
        assert_eq!(
            HostIncarnationLease::claim(&anchor).unwrap().incarnation(),
            Incarnation(2)
        );
    }

    #[tokio::test]
    async fn boot_registration_refuses_another_run_before_creating_journal() {
        let owned = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let owned_anchor =
            tidepool_atomic_write::DirectoryAnchor::open_existing(owned.path()).unwrap();
        let other_anchor =
            tidepool_atomic_write::DirectoryAnchor::open_existing(other.path()).unwrap();
        let (_lease, registration) = HostIncarnationLease::claim(&owned_anchor)
            .unwrap()
            .into_boot_registration();
        let result = exomonad_actor::ActorRecoveryJournal::open_for_boot(
            &other_anchor,
            "actors.jsonl",
            false,
            Box::new(registration),
        )
        .await;
        assert!(result.is_err());
        assert!(!other.path().join("actors.jsonl").exists());
    }

    #[test]
    fn rejects_unknown_versions_without_replacing_them() {
        let runtime = tempfile::tempdir().unwrap();
        let path = runtime.path().join("host-incarnation.json");
        let original = br#"{"version":99,"last_incarnation":12}"#;
        fs::write(&path, original).unwrap();

        let error = HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap(),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported durable state version"));
        assert_eq!(fs::read(path).unwrap(), original);
    }
}
