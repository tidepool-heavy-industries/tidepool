//! Shared plumbing for this crate's durable-store modules — [`registry`],
//! [`binding`](crate::binding), and [`journal`](crate::journal) — plus their
//! callers outside this crate (`tidepool-agent`'s spawn saga,
//! `tidepool-handlers`' repository-event handler). Both `now_ms` and
//! `storage_failure` used to be copied independently at each call site; this
//! module is the one place either is defined now.
//!
//! [`DurableJsonDir`] is the same consolidation for [`registry`] and
//! [`binding`](crate::binding) specifically: both keep one JSON file per
//! path-safe id under a directory, written atomically and read back whole or
//! by directory scan. Path construction, id-safety validation, raw JSON
//! load, and error mapping were each implemented twice; only the
//! (de)serialized shape and the domain rules around it — a registry's single
//! receipt per id versus a binding table's per-id lease HISTORY, its
//! rollback-on-persist-failure discipline — differ, and those stay in the
//! owning module.
//!
//! [`registry`]: crate::registry
//!
//! ## The `now_ms` behavior this consolidation chose
//!
//! Four independent copies existed before this consolidation: `tidepool-agent`'s
//! spawn saga, this crate's `registry.rs`, this crate's `journal.rs`, and
//! `tidepool-handlers`' repository-event handler (the `Tick` `firedAtMs`
//! stamp). Three of the four already agreed on PANICKING on a pre-epoch
//! clock; the fourth — the event handler's Tick stamp, documented there as
//! "an observability stamp only" — silently degraded to `0` instead. That
//! divergence is a bug surfaced by consolidation, not one this module
//! preserves: every caller now panics. A pre-epoch wall clock is a broken
//! machine no caller can act on, and a Tick silently stamped `0` on a broken
//! clock is a worse failure — a wrong-looking-right observability record —
//! than losing that one tick loudly.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::WorktreeError;
use crate::id::WorktreeId;

/// Current time as Unix epoch milliseconds.
///
/// # Panics
///
/// If the system clock reads before the Unix epoch — see the module docs for
/// why every caller here deliberately panics rather than degrading.
pub fn now_ms() -> i64 {
    #[allow(clippy::expect_used, reason = "system clock is before the Unix epoch")]
    {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_millis() as i64
    }
}

/// Build a [`WorktreeError::StorageFailure`] naming the path that actually
/// failed, from any underlying error with a `Display` impl (`std::io::Error`
/// for I/O, `serde_json::Error` for a corrupt record).
pub fn storage_failure(path: &Path, detail: impl std::fmt::Display) -> WorktreeError {
    WorktreeError::StorageFailure {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}

/// A directory holding one JSON file per path-safe id — the shared mechanism
/// behind [`crate::registry::WorktreeRegistry`] (one receipt per id) and
/// [`crate::binding::BindingTable`] (one lease-history array per id). Handles
/// path construction, id-safety validation, raw bytes in/out, and error
/// mapping; the caller owns (de)serialization and any domain rule (rollback,
/// history semantics, locking) around it.
#[derive(Clone, Debug)]
pub struct DurableJsonDir {
    dir: PathBuf,
}

impl DurableJsonDir {
    /// Open (creating if absent) a JSON-file directory rooted at `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| storage_failure(&dir, e))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path a path-safe `id` is stored at.
    ///
    /// # Panics (debug only)
    /// `id` must already be validated path-safe at the wire boundary
    /// ([`WorktreeId::is_path_safe`]) — this is the backstop that keeps a
    /// missed boundary from becoming a path escape instead of a loud bug,
    /// not the validation itself.
    pub fn path_for(&self, id: &str) -> PathBuf {
        debug_assert!(
            WorktreeId::is_path_safe(id),
            "id {id:?} is not path-safe — a wire boundary failed to validate"
        );
        self.dir.join(format!("{id}.json"))
    }

    /// Whether `id` currently has a file on disk.
    pub fn exists(&self, id: &str) -> bool {
        self.path_for(id).exists()
    }

    /// Durably write `bytes` for `id` — a temp file in this directory,
    /// fsynced, then renamed over the target ([`tidepool_atomic_write::write_durable`]).
    pub fn write(&self, id: &str, bytes: &[u8]) -> Result<(), WorktreeError> {
        tidepool_atomic_write::write_durable(&self.path_for(id), bytes)
            .map_err(|e| storage_failure(&e.path, e.source))
    }

    /// Read one id's raw bytes back. `Ok(None)` when `id` has no file —
    /// distinct from a read error, so a caller can tell "never written" from
    /// "storage is broken".
    pub fn read(&self, id: &str) -> Result<Option<Vec<u8>>, WorktreeError> {
        let path = self.path_for(id);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage_failure(&path, e)),
        }
    }

    /// Every `*.json` file under this directory, as `(path, bytes)` — path
    /// kept alongside the bytes so a caller's deserialize failure can still
    /// name the exact file. Unordered; callers impose whatever order their
    /// domain needs.
    pub fn read_all(&self) -> Result<Vec<(PathBuf, Vec<u8>)>, WorktreeError> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(|e| storage_failure(&self.dir, e))? {
            let entry = entry.map_err(|e| storage_failure(&self.dir, e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|e| storage_failure(&path, e))?;
            out.push((path, bytes));
        }
        Ok(out)
    }
}
