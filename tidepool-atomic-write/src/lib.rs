//! The one same-directory atomic write-then-rename helper.
//!
//! Six durable on-disk stores across the workspace (the worktree registry,
//! the agent binding table, the self-harness checkpoint, the run lease, the
//! toolchain stamp, and a session's compiled-module cache) each wrote the
//! same way by hand: a temp file in the SAME directory as the target (so the
//! final rename stays on one filesystem and is atomic), then a rename over
//! the target — so a reader (this process's own next boot, or a concurrent
//! one) can never observe a torn write. This crate is that one mechanism,
//! in two durability tiers:
//!
//! - [`write_durable`] — file fsync + best-effort parent-directory fsync, so
//!   the rename itself survives a crash. Use for state a restart must be
//!   able to trust: registry rows, leases, checkpoints, toolchain stamps.
//! - [`write_best_effort`] — no fsync at all; only the rename's atomicity
//!   (never a torn read) is kept. Use for regenerable caches, where a lost
//!   write on a crash is just a future cache miss, not data loss.
//!
//! Both use [`tempfile::NamedTempFile`] for the temp file itself, which
//! picks a unique name per call — no caller needs to invent its own, and no
//! caller can collide with a sibling writer racing on the same target path.
//!
//! Deliberately NOT here: [`std::fs::hard_link`]-based exclusive-claim
//! writes (an ordinary rename overwrites; a hard link fails loud when the
//! target already exists) — that is a different primitive for a different
//! job and stays with its one caller.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::io::Write;
use std::path::{Path, PathBuf};

/// An atomic-write failure, naming the path the failing step actually
/// touched — the target's own DIRECTORY when the temp file itself could not
/// be created there (e.g. the directory is read-only or missing), the
/// target path itself for every later step (write, fsync, rename). Callers
/// that report "the write failed" want the site of the failure, not always
/// the final target — a directory-creation failure naming a file inside it
/// that was never reached is a worse diagnostic, not a better one.
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

/// Write `bytes` to `path` atomically and durably: a uniquely-named temp
/// file in `path`'s own directory, fsynced, renamed over `path`, then the
/// directory itself is best-effort fsynced so the rename survives a crash
/// too (not fatal if the platform/filesystem doesn't support fsync on a
/// directory handle — that only widens the crash window for the rename
/// itself, not the write's atomicity).
///
/// Does not create `path`'s parent directory — callers that need one
/// created call `std::fs::create_dir_all` themselves first.
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
    if let Ok(dirf) = std::fs::File::open(dir) {
        let _ = dirf.sync_all();
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
