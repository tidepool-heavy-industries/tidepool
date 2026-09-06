//! Same-directory atomic replacement, with strict and best-effort durability tiers.
//!
//! [`write_durable`] syncs the file and containing directory and reports every
//! failure. An error after rename can leave the new contents visible: callers
//! must treat the result as uncertain, not proof that publication did not occur.
//! [`write_best_effort`] preserves atomic replacement without requiring storage sync.
//! Neither writer creates its parent directory. Owners creating persistent storage
//! use [`create_dir_all_durable`] before publishing entries beneath new directories.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::io::Write;
use std::path::{Path, PathBuf};

/// An atomic-write failure naming the path touched by the failing operation.
/// Directory creation/open/sync failures name that directory. A failed sync
/// after publication does not imply that the file or directories are absent.
#[derive(Debug)]
pub struct WriteError {
    pub path: PathBuf,
    pub source: std::io::Error,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<WriteError> for std::io::Error {
    fn from(e: WriteError) -> Self {
        e.source
    }
}

/// Atomically replace a file, syncing its contents and then its parent directory.
/// Parent-directory open and sync failures are reported, including after rename
/// has made the new contents visible. An error does not roll back publication.
/// The parent must already exist; use [`create_dir_all_durable`] when creating it.
pub fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let dir = parent_dir(path);
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| WriteError {
        path: dir.to_path_buf(),
        source,
    })?;
    tmp.write_all(bytes).map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    tmp.as_file().sync_all().map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    tmp.persist(path).map_err(|e| WriteError {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    sync_parent_directory(path)
}

/// Sync the directory containing a published path. The caller must sync the
/// file first and durably establish newly created ancestry separately. Errors
/// can occur after the path becomes visible; they do not authorize blind retry.
pub fn sync_parent_directory(path: &Path) -> Result<(), WriteError> {
    sync_directory(parent_dir(path))
}

/// Sync one existing directory, reporting unsupported operations and I/O failures.
/// This persists its entries, not file contents or links to this directory from
/// its own parent. The caller owns concurrent mutation and publication ordering.
fn sync_directory(path: &Path) -> Result<(), WriteError> {
    let directory = std::fs::File::open(path).map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    if !directory
        .metadata()
        .map_err(|source| WriteError {
            path: path.to_path_buf(),
            source,
        })?
        .is_dir()
    {
        return Err(WriteError {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "directory sync requires a directory",
            ),
        });
    }
    directory.sync_all().map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })
}

/// Create a directory hierarchy and sync it and every ancestor on the supplied
/// path, deepest first. This also repairs the persistence ordering on a later
/// invocation after an earlier sync failed with already-visible directories.
///
/// The caller must own the hierarchy against concurrent rename/removal. Existing
/// symlink targets and their ancestry must already be durably established; this
/// does not create symlinks or resolve a separate external target hierarchy.
/// Errors may leave directories present but not durably confirmed. No rollback
/// is attempted. Filesystem/platform directory-sync failures are never ignored.
pub fn create_dir_all_durable(path: &Path) -> Result<(), WriteError> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    std::fs::create_dir_all(path).map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    let absolute = std::path::absolute(path).map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    for directory in absolute.ancestors() {
        sync_directory(directory)?;
    }
    Ok(())
}

/// Write `bytes` to `path` atomically WITHOUT fsync: a uniquely-named temp
/// file in `path`'s own directory, renamed over `path`. The rename alone
/// still means a reader never observes a torn write — only durability
/// across a crash is sacrificed. Use for regenerable caches.
///
/// Does not create `path`'s parent directory — see [`write_durable`].
pub fn write_best_effort(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let dir = parent_dir(path);
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| WriteError {
        path: dir.to_path_buf(),
        source,
    })?;
    tmp.write_all(bytes).map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    tmp.persist(path).map_err(|e| WriteError {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_durable_round_trips_and_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        write_durable(&path, b"v1").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v1");
        write_durable(&path, b"v2-longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v2-longer");
    }

    #[test]
    fn write_best_effort_round_trips_and_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        write_best_effort(&path, b"v1").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v1");
        write_best_effort(&path, b"v2-longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v2-longer");
    }

    /// A stray leftover temp file (a prior process killed mid-write) must
    /// never be mistaken for the target, and a fresh write must ignore it
    /// rather than collide with it — proves the per-call unique tmp name.
    #[test]
    fn a_stray_leftover_tmp_file_does_not_collide_with_a_fresh_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        write_durable(&path, b"v1").unwrap();

        let leftover = dir.path().join("f.txt.tmp-leftover");
        std::fs::write(&leftover, b"garbage, never persisted").unwrap();

        write_durable(&path, b"v2").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"v2");
        assert!(leftover.exists(), "a fresh write must not touch it");
    }

    #[test]
    fn concurrent_writers_never_observe_a_torn_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        write_durable(&path, b"seed").unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let payload = format!("writer-{i}").repeat(50);
                    write_durable(&path, payload.as_bytes()).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // Whatever landed must be exactly ONE writer's complete payload —
        // never a mix of two, which a non-atomic write could produce.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            (0..8).any(|i| contents == format!("writer-{i}").repeat(50)),
            "final content must be exactly one writer's complete payload: {contents:?}"
        );
    }

    /// A failure that happens before the temp file can even be created (the
    /// directory is unwritable) must name the DIRECTORY, not the target file
    /// that was never reached — otherwise a caller reports a misleading
    /// "failed to write <file>" for a problem that is actually about the
    /// directory it lives in.
    #[cfg(unix)]
    #[test]
    fn a_failure_creating_the_temp_file_names_the_directory_not_the_target() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("readonly");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_durable(&path, b"v1");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        match result {
            Err(e) => assert_eq!(
                e.path, dir,
                "the failure must name the directory being written to"
            ),
            Ok(()) => {
                eprintln!("SKIPPED: write succeeded despite chmod 0o555 (likely running as root)")
            }
        }
    }
}
