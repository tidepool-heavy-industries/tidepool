//! Cooperative Git admission for one persistent lock inside Git common metadata.
//!
//! The lock lives *inside* Git's common directory, so linked worktrees and
//! separate Exomonad processes open the same inode. A different mounted view
//! with the same printed path opens a different directory and cannot borrow
//! its permit. Native application writers are sampled at their own boundary;
//! arbitrary external editors do not participate in this protocol.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

const ADMISSION_DIR: &std::ffi::CStr = c"exomonad-admission";
const LOCK_NAME: &std::ffi::CStr = c"lock";

/// The transaction's declared relationship to shared Git state. Read permits
/// coexist; write and capture permits exclude all cooperating readers/writers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitAccess {
    Read,
    Write,
    Capture,
}

/// Kernel identity of the opened lock file, independent of its path spelling
/// in a host or mounted worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct GitBacking {
    pub device: u64,
    pub inode: u64,
}

pub(crate) struct GitPermit {
    _file: File,
}

/// The exact lock inode selected through an opened common directory. A view's
/// directory stat alone cannot prove two commands would flock the same inode.
pub(crate) struct GitCandidate {
    file: File,
    pub backing: GitBacking,
}

impl GitBacking {
    fn of_lock(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Git admission lock is not a file",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

fn open_lock(common: &File) -> io::Result<File> {
    if !common.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Git common directory is not a directory",
        ));
    }
    // SAFETY: both names are fixed NUL-terminated constants, and `common`
    // pins the exact metadata directory selected by the caller's view.
    let created = unsafe { libc::mkdirat(common.as_raw_fd(), ADMISSION_DIR.as_ptr(), 0o700) };
    if created < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returns a new descriptor for this exact child directory.
    let directory = unsafe {
        libc::openat(
            common.as_raw_fd(),
            ADMISSION_DIR.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if directory < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat transferred descriptor ownership.
    let directory = unsafe { File::from_raw_fd(directory) };
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Git admission directory is not private to this user",
        ));
    }
    // SAFETY: openat returns a new descriptor for the persistent lock inode.
    let file = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            LOCK_NAME.as_ptr(),
            libc::O_CREAT | libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if file < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat transferred descriptor ownership.
    let file = unsafe { File::from_raw_fd(file) };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Git admission lock file is not private to this user",
        ));
    }
    Ok(file)
}

impl GitCandidate {
    pub(crate) fn open(common: &File) -> io::Result<Self> {
        let file = open_lock(common)?;
        let backing = GitBacking::of_lock(&file)?;
        Ok(Self { file, backing })
    }
}

impl GitPermit {
    /// Acquire a permit against the actual opened Git backing. `timeout=ZERO`
    /// refuses contention immediately. Call bounded waits only on blocking
    /// workers; this method never borrows a VM, worktree registry, or async lock.
    pub(crate) fn acquire(
        candidate: GitCandidate,
        access: GitAccess,
        timeout: Duration,
    ) -> io::Result<Self> {
        let GitCandidate { file, backing } = candidate;
        let operation = match access {
            GitAccess::Read => libc::LOCK_SH | libc::LOCK_NB,
            GitAccess::Write | GitAccess::Capture => libc::LOCK_EX | libc::LOCK_NB,
        };
        let started = Instant::now();
        loop {
            // SAFETY: flock acts on the owned persistent lock descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
                return Ok(Self { _file: file });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("Git {access:?} admission busy for backing {backing:?}"),
                ));
            }
            #[allow(
                clippy::disallowed_methods,
                reason = "bounded lock wait on a synchronous Git worker"
            )]
            std::thread::sleep(
                Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
            );
        }
    }
}
