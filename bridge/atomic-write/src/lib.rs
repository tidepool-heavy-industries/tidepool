//! Same-directory atomic publication, with replacement and exclusive creation.
//!
//! [`write_durable`] syncs the file and containing directory and reports every
//! failure; its legacy error type cannot distinguish a post-rename sync error.
//! Use [`stage_durable`] when publication is a commit point: its staged handle
//! separates preparation from rename, and [`PublishError`] distinguishes an
//! unpublished failure from a visible publication whose durability is unconfirmed.
//! [`write_best_effort`] preserves atomic replacement without requiring storage sync.
//! Neither writer creates its parent directory. Owners creating persistent storage
//! use [`DirectoryAnchor`] before publishing entries beneath new directories.

use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

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
        Self::new(e.source.kind(), e)
    }
}

/// A write whose bytes are complete and synced but whose target has not yet
/// been replaced. The temporary file is in the target's directory so publish
/// remains an atomic same-filesystem rename.
pub struct StagedDurableWrite {
    target: PathBuf,
    temp: NamedTempFile,
}

/// A known-visible publication whose parent-directory durability can be
/// confirmed again without repeating its rename.
#[derive(Clone, Debug)]
pub struct PublishedWrite {
    target: PathBuf,
}

impl PublishedWrite {
    /// Retry durability confirmation for this publication. Callers must
    /// serialize writes to the same target until confirmation succeeds.
    pub fn confirm_durability(&self) -> Result<(), WriteError> {
        sync_parent_directory(&self.target)
    }

    /// The path made visible by this publication.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.target
    }
}

/// Failure while publishing a staged durable write. `PublishedDurabilityUnconfirmed`
/// means rename succeeded and the new file is visible; retry only the supplied
/// receipt's directory sync, never the publication itself.
#[derive(Debug)]
pub enum PublishError {
    BeforeRename(WriteError),
    PublishedDurabilityUnconfirmed {
        publication: PublishedWrite,
        source: WriteError,
    },
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeRename(error) => write!(f, "publication did not occur: {error}"),
            Self::PublishedDurabilityUnconfirmed { source, .. } => {
                write!(
                    f,
                    "publication is visible but durability is unconfirmed: {source}"
                )
            }
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BeforeRename(error) => Some(error),
            Self::PublishedDurabilityUnconfirmed { source, .. } => Some(source),
        }
    }
}

/// Prepare a durable write without making the target visible. The file is
/// written and fsynced in a unique temporary file beside `path`; dropping the
/// returned value before [`StagedDurableWrite::publish`] removes that temp.
pub fn stage_durable(path: &Path, bytes: &[u8]) -> Result<StagedDurableWrite, WriteError> {
    let dir = parent_dir(path);
    let mut temp = NamedTempFile::new_in(dir).map_err(|source| WriteError {
        path: dir.to_path_buf(),
        source,
    })?;
    temp.write_all(bytes).map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    temp.as_file().sync_all().map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(StagedDurableWrite {
        target: path.to_path_buf(),
        temp,
    })
}

impl StagedDurableWrite {
    /// Atomically replace the target and confirm the parent directory. A
    /// post-rename sync error carries a receipt for retrying only that sync.
    pub fn publish(self) -> Result<PublishedWrite, PublishError> {
        let Self { target, temp } = self;
        temp.persist(&target).map_err(|error| {
            PublishError::BeforeRename(WriteError {
                path: target.clone(),
                source: error.error,
            })
        })?;
        let publication = PublishedWrite { target };
        publication.confirm_durability().map_err(|source| {
            PublishError::PublishedDurabilityUnconfirmed {
                publication: publication.clone(),
                source,
            }
        })?;
        Ok(publication)
    }
}

/// Atomically replace a file, syncing its contents and then its parent directory.
/// Parent-directory open and sync failures are reported, including after rename
/// has made the new contents visible. Use [`stage_durable`] to distinguish that
/// post-rename case. The parent must already exist; use
/// [`DirectoryAnchor::create_dir_all`] when creating it.
pub fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    stage_durable(path, bytes)?
        .publish()
        .map(|_| ())
        .map_err(|error| match error {
            PublishError::BeforeRename(error)
            | PublishError::PublishedDurabilityUnconfirmed { source: error, .. } => error,
        })
}

/// Durably create a file only if its name is absent. Returns `true` when this
/// call published the file and `false` when another publisher already owns the
/// name. A failed directory sync can occur after publication, so errors do not
/// prove the file is absent. The parent directory must already exist.
pub fn write_durable_new(path: &Path, bytes: &[u8]) -> Result<bool, WriteError> {
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
    match tmp.persist_noclobber(path) {
        Ok(_) => {
            sync_parent_directory(path)?;
            Ok(true)
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            // The winner may still be syncing the directory. Confirm its name
            // before returning a durable existing-file observation.
            sync_parent_directory(path)?;
            Ok(false)
        }
        Err(error) => Err(WriteError {
            path: path.to_path_buf(),
            source: error.error,
        }),
    }
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
fn open_directory(path: &Path) -> Result<std::fs::File, WriteError> {
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
    Ok(directory)
}

fn sync_directory(path: &Path) -> Result<(), WriteError> {
    open_directory(path)?
        .sync_all()
        .map_err(|source| WriteError {
            path: path.to_path_buf(),
            source,
        })
}

/// An existing, durably established directory that bounds directory creation.
///
/// The owner must have established the anchor's own link from its parent before
/// opening it. This is a durability boundary, not filesystem access authority.
/// The owner must prevent concurrent rename/removal within the hierarchy.
/// Existing symlink targets and their ancestry must already be durable; this
/// does not establish a separate external target hierarchy.
#[derive(Clone, Debug)]
pub struct DirectoryAnchor {
    path: PathBuf,
}

impl DirectoryAnchor {
    /// Open an existing stable directory without creating it or syncing its
    /// ancestors. The caller asserts its parent link is already durable.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self, WriteError> {
        let requested = path.as_ref();
        let path = std::fs::canonicalize(requested).map_err(|source| WriteError {
            path: requested.to_path_buf(),
            source,
        })?;
        open_directory(&path)?;
        Ok(Self { path })
    }

    /// The absolute directory at which durability confirmation stops.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve an ordinary relative path within this directory without mutation.
    /// This validates lexical containment, not symlink access authority.
    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf, WriteError> {
        Ok(self.path.join(self.normalize_relative(relative.as_ref())?))
    }

    /// Establish a child hierarchy before opening it as a stable boundary.
    /// A failed creation never produces an anchor for a merely visible child.
    pub fn child(&self, relative: impl AsRef<Path>) -> Result<Self, WriteError> {
        Self::open_existing(self.create_dir_all(relative)?)
    }

    fn normalize_relative(&self, relative: &Path) -> Result<PathBuf, WriteError> {
        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(component) => normalized.push(component),
                std::path::Component::CurDir => {}
                _ => {
                    return Err(WriteError {
                        path: relative.to_path_buf(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "directory anchor requires a relative path without parent components",
                        ),
                    });
                }
            }
        }
        Ok(normalized)
    }

    /// Create a relative hierarchy and sync every directory from its leaf back
    /// through this anchor, deepest first, even when all directories exist.
    ///
    /// Absolute paths and parent components are rejected before mutation. An
    /// empty path confirms this anchor. Failures can leave visible directories
    /// whose durability is unconfirmed; retry with the same stable anchor and
    /// relative path. Never promote a failed creation to a new anchor.
    /// Directory open and sync errors, including unsupported operations, are
    /// reported without rollback or suppression.
    pub fn create_dir_all(&self, relative: impl AsRef<Path>) -> Result<PathBuf, WriteError> {
        let normalized = self.normalize_relative(relative.as_ref())?;
        let path = self.path.join(&normalized);
        std::fs::create_dir_all(&path).map_err(|source| WriteError {
            path: path.clone(),
            source,
        })?;
        for directory in normalized.ancestors() {
            sync_directory(&self.path.join(directory))?;
        }
        Ok(path)
    }
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
    fn io_conversion_preserves_failed_path_and_error_kind() {
        let error: std::io::Error = WriteError {
            path: PathBuf::from("/owned/runtime"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }
        .into();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("/owned/runtime"));
        assert!(error
            .get_ref()
            .unwrap()
            .downcast_ref::<WriteError>()
            .is_some());
    }

    #[test]
    fn directory_anchor_requires_an_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let absent = root.path().join("absent");
        let error = DirectoryAnchor::open_existing(&absent).unwrap_err();
        assert_eq!(error.path, absent);
        assert_eq!(error.source.kind(), std::io::ErrorKind::NotFound);
        assert!(!absent.exists());
        let file = root.path().join("file");
        std::fs::write(&file, b"value").unwrap();
        let error = DirectoryAnchor::open_existing(&file).unwrap_err();
        assert_eq!(error.path, file);
        assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn directory_anchor_rejects_escaping_paths_before_creation() {
        let root = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(root.path()).unwrap();
        for relative in [
            Path::new("new/../escaped"),
            Path::new("../escaped"),
            root.path(),
        ] {
            let error = anchor.create_dir_all(relative).unwrap_err();
            assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidInput);
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert_eq!(anchor.create_dir_all("").unwrap(), anchor.path());
        assert_eq!(
            anchor.create_dir_all("./new/deep").unwrap(),
            anchor.path().join("new/deep")
        );
    }

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
    fn staged_write_leaves_target_old_until_publish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest");
        write_durable(&path, b"old").unwrap();
        let staged = stage_durable(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        staged.publish().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn rename_failure_is_typed_as_not_published_and_keeps_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let staged = stage_durable(&target, b"new").unwrap();
        assert!(matches!(
            staged.publish(),
            Err(PublishError::BeforeRename(_))
        ));
        assert!(target.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_durable_new_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected.txt");
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let payload = format!("writer-{index}");
                    (index, write_durable_new(&path, payload.as_bytes()).unwrap())
                })
            })
            .collect();
        let winners: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter_map(|(index, created)| created.then_some(index))
            .collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("writer-{}", winners[0])
        );
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
