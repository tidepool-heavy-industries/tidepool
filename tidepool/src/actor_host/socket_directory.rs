//! Exclusive socket-path custody. Hosted submission is irreversible without exact
//! process and accepted-work cleanup evidence; current tmux retirement has none.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct SocketDirectory {
    path: PathBuf,
    state: State,
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    Unsubmitted,
    RetainedUnconfirmed,
    Released,
}

impl SocketDirectory {
    pub(super) fn create(path: PathBuf) -> std::io::Result<Self> {
        // Only exclusive successful creation grants deletion custody.
        std::fs::create_dir(&path)?;
        Ok(Self {
            path,
            state: State::Unsubmitted,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn work_may_exist(&mut self) {
        self.state = State::RetainedUnconfirmed;
    }

    pub(super) fn release(mut self) -> std::io::Result<()> {
        if self.state != State::Unsubmitted {
            return Err(std::io::Error::other(format!(
                "socket directory {} retained: exact process and accepted hosted work cleanup is unconfirmed",
                self.path.display()
            )));
        }
        self.remove_once()
    }

    fn remove_once(&mut self) -> std::io::Result<()> {
        // A failed removal may be partial. Neither release nor Drop retries it.
        self.state = State::RetainedUnconfirmed;
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {
                self.state = State::Released;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.state = State::Released;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        match self.state {
            State::Unsubmitted => {
                if let Err(error) = self.remove_once() {
                    tracing::warn!(path = %self.path.display(), %error, "unsubmitted socket directory cleanup failed; no retry");
                }
            }
            State::RetainedUnconfirmed => {
                tracing::warn!(path = %self.path.display(), "socket directory retained; cleanup is unconfirmed and no deletion is attempted");
            }
            State::Released => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_directory_unsubmitted_drop_removes_owned_named_socket() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("socket-root");
        let guard = SocketDirectory::create(path.clone()).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(guard.path().join("host-tools.sock")).unwrap();
        drop(listener);
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn socket_directory_collision_never_acquires_deletion_custody() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preexisting");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("owner"), b"someone else").unwrap();
        assert_eq!(
            SocketDirectory::create(path.clone()).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(path.join("owner")).unwrap(), b"someone else");
    }

    #[test]
    fn socket_directory_submitted_drop_and_release_retain_paths() {
        let root = tempfile::tempdir().unwrap();
        for explicit in [false, true] {
            let path = root.path().join(format!("submitted-{explicit}"));
            let mut guard = SocketDirectory::create(path.clone()).unwrap();
            let listener =
                std::os::unix::net::UnixListener::bind(path.join("host-tools.sock")).unwrap();
            guard.work_may_exist();
            drop(listener);
            if explicit {
                assert!(guard
                    .release()
                    .unwrap_err()
                    .to_string()
                    .contains("unconfirmed"));
            } else {
                drop(guard);
            }
            assert!(path.join("host-tools.sock").exists());
        }
    }

    #[test]
    fn socket_directory_failed_removal_is_not_retried_by_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned");
        let mut guard = SocketDirectory::create(path.clone()).unwrap();
        // Deterministic failure, even under root; no process-wide fault injection.
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, b"not a directory").unwrap();
        assert!(guard.remove_once().is_err());
        assert_eq!(guard.state, State::RetainedUnconfirmed);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("retained"), b"must remain").unwrap();
        drop(guard);
        assert!(path.join("retained").exists());
    }
}
