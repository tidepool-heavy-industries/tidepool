//! Durable epoch allocation for exact local actor identities.
//!
//! Ractor process IDs are local to one daemon process and may be reused after
//! restart. This owner claims one monotonically increasing incarnation while
//! holding a lifetime file lock. Every actor spawned by that daemon shares the
//! epoch, so an old `(ActorId, Incarnation)` can never authorize a new process.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tidepool_actor::Incarnation;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IncarnationState {
    version: u32,
    last_incarnation: u64,
}

/// Exclusive claim on one Shoal host incarnation.
#[derive(Debug)]
pub(super) struct HostIncarnationLease {
    incarnation: Incarnation,
    _owner_lock: HostRunLock,
}

/// Shared host/maintenance exclusion; maintenance never allocates an incarnation.
#[derive(Debug)]
pub(super) struct HostRunLock(File);
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
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::other(format!(
                "another Shoal host or maintenance operation already owns {}",
                run_root.display()
            ))),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}
impl Drop for HostRunLock {
    fn drop(&mut self) {
        // Concurrent fork may temporarily inherit the open description.
        // Unlock ends ownership without waiting for that child to exec.
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "Shoal host owner unlock failed");
        }
    }
}

impl HostIncarnationLease {
    pub(super) fn claim(run_root: &Path) -> io::Result<Self> {
        fs::create_dir_all(run_root)?;
        let owner_lock = HostRunLock::claim(run_root, true)?;

        let state_path = run_root.join("host-incarnation.json");
        let previous = read_previous(&state_path)?;
        let next = previous.checked_add(1).ok_or_else(|| {
            io::Error::other(format!(
                "Shoal host incarnation exhausted at {}",
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
            _owner_lock: owner_lock,
        })
    }

    pub(super) const fn incarnation(&self) -> Incarnation {
        self.incarnation
    }
}

fn read_previous(path: &Path) -> io::Result<u64> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
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
    Ok(state.last_incarnation)
}

fn invalid_state(path: &Path, detail: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "invalid Shoal host incarnation state {}: {detail}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_monotonically_increasing_incarnations() {
        let runtime = tempfile::tempdir().unwrap();
        let first = HostIncarnationLease::claim(runtime.path()).unwrap();
        assert_eq!(first.incarnation(), Incarnation(1));
        drop(first);

        let second = HostIncarnationLease::claim(runtime.path()).unwrap();
        assert_eq!(second.incarnation(), Incarnation(2));
    }

    #[test]
    fn refuses_a_second_live_owner() {
        let runtime = tempfile::tempdir().unwrap();
        let _first = HostIncarnationLease::claim(runtime.path()).unwrap();
        let error = HostIncarnationLease::claim(runtime.path()).unwrap_err();
        assert!(error.to_string().contains("already owns"));
    }

    #[test]
    fn rejects_unknown_versions_without_replacing_them() {
        let runtime = tempfile::tempdir().unwrap();
        let path = runtime.path().join("host-incarnation.json");
        let original = br#"{"version":99,"last_incarnation":12}"#;
        fs::write(&path, original).unwrap();

        let error = HostIncarnationLease::claim(runtime.path()).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported durable state version"));
        assert_eq!(fs::read(path).unwrap(), original);
    }
}
