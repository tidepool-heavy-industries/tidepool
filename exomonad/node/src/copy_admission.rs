//! Serialize managed full-tree copies on the destination filesystem.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::FlockOperation;

/// Conservative operational defaults for a full-tree copy. A caller may
/// configure import limits, but compaction and retirement use these values.
pub const DEFAULT_MAX_COPY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const DEFAULT_MIN_FREE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// A cross-process admission lease. The lock file is persistent: deleting it
/// could let later processes lock a different inode while an older copy runs.
pub struct CopyAdmission {
    _file: File,
    pub free_bytes: u64,
    budget_bytes: u64,
    destination_device: u64,
}

impl CopyAdmission {
    /// Check a conservative physical-copy budget while holding a filesystem
    /// lock, then keep the lock through copy, verification and partial cleanup.
    /// A zero timeout refuses contention immediately. Call bounded waits from
    /// a blocking task, never an asynchronous executor thread.
    pub fn acquire(
        destination: &Path,
        budget_bytes: u64,
        max_bytes: u64,
        min_free_bytes: u64,
        timeout: Duration,
    ) -> io::Result<Self> {
        if budget_bytes > max_bytes {
            return Err(io::Error::other(format!(
                "copy admission refused before copy: budget bytes={budget_bytes}, max bytes={max_bytes}"
            )));
        }
        let destination_device = fs::metadata(destination)?.dev();
        let lock_path = lock_path(destination_device)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::other(format!(
                "copy admission lock file is not private: {}",
                lock_path.display()
            )));
        }
        let started = Instant::now();
        loop {
            match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(error) if io::Error::from(error).kind() == io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!("copy admission busy for destination filesystem {destination_device}"),
                        ));
                    }
                    #[allow(
                        clippy::disallowed_methods,
                        reason = "bounded wait on a synchronous copy worker"
                    )]
                    std::thread::sleep(
                        Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        if fs::metadata(destination)?.dev() != destination_device {
            return Err(io::Error::other(
                "copy destination changed filesystems during admission",
            ));
        }
        let free = rustix::fs::statvfs(destination).map_err(io::Error::from)?;
        let free_bytes = free.f_bavail.saturating_mul(free.f_frsize);
        if free_bytes < budget_bytes.saturating_add(min_free_bytes) {
            return Err(io::Error::other(format!(
                "copy admission refused before copy: budget bytes={budget_bytes}, free bytes={free_bytes}, reserve bytes={min_free_bytes}"
            )));
        }
        Ok(Self {
            _file: file,
            free_bytes,
            budget_bytes,
            destination_device,
        })
    }

    /// The copy owner checks this at its entry point so a caller cannot pass
    /// a lease for a smaller copy or a different destination filesystem.
    pub fn covers(&self, destination: &Path, budget_bytes: u64) -> io::Result<()> {
        if fs::metadata(destination)?.dev() != self.destination_device
            || budget_bytes > self.budget_bytes
        {
            return Err(io::Error::other(
                "copy admission does not cover destination and budget",
            ));
        }
        Ok(())
    }
}

fn lock_path(device: u64) -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::other("HOME is required for persistent copy admission"))?;
    let directory = PathBuf::from(home).join(".local/state/tidepool/copy-admission");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other(format!(
            "copy admission directory is not private: {}",
            directory.display()
        )));
    }
    Ok(directory.join(format!(
        "uid-{}-device-{device:x}.lock",
        rustix::process::geteuid().as_raw()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    use std::sync::{mpsc, Arc, Barrier};

    #[test]
    fn concurrent_copy_refuses_and_retries_after_holder_exits() {
        let destination = tempfile::tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let (release, wait_for_release) = mpsc::channel();
        let path = destination.path().to_path_buf();
        let child_barrier = barrier.clone();
        let holder = std::thread::spawn(move || {
            let first = CopyAdmission::acquire(&path, 1, u64::MAX, 1, Duration::ZERO).unwrap();
            child_barrier.wait();
            wait_for_release.recv().unwrap();
            drop(first);
        });
        barrier.wait();
        let error = CopyAdmission::acquire(destination.path(), 1, u64::MAX, 1, Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        release.send(()).unwrap();
        holder.join().unwrap();
        CopyAdmission::acquire(destination.path(), 1, u64::MAX, 1, Duration::ZERO).unwrap();
    }

    #[test]
    fn process_interruption_releases_same_lock_inode() {
        let destination = tempfile::tempdir().unwrap();
        let guard =
            CopyAdmission::acquire(destination.path(), 1, u64::MAX, 1, Duration::ZERO).unwrap();
        drop(guard);
        let lock = lock_path(fs::metadata(destination.path()).unwrap().dev()).unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "short fixture process for interruption proof"
        )]
        let mut child = Command::new("flock")
            .args([
                "-x",
                "-F",
                lock.to_str().unwrap(),
                "sh",
                "-c",
                "printf 'ready\\n'; read line",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "ready\n");
        assert_eq!(
            CopyAdmission::acquire(destination.path(), 1, u64::MAX, 1, Duration::ZERO)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        CopyAdmission::acquire(destination.path(), 1, u64::MAX, 1, Duration::ZERO).unwrap();
    }
}
