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
        if self.state == State::Unsubmitted {
            if let Err(error) = self.remove_once() {
                tracing::warn!(path = %self.path.display(), %error, "unsubmitted socket directory cleanup failed; no retry");
            }
        }
    }
}
