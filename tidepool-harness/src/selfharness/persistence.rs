//! W3: local-file persistence for the self-iterating harness (D5,
//! 08-wave1-correctness.md "Persistence = local files" — locked). Two
//! pieces, both plain files, no DB:
//!
//! - **State → json**: [`save_state`]/[`load_state`] round-trip the
//!   loop-boundary `State` JSON ([`crate::selfharness::state_cross::state_out`])
//!   through a file, so [`crate::selfharness::driver::SelfHarnessDriver::run_loop`]
//!   can restore it on a fresh process start rather than always falling
//!   back to `initialState`. `save_state` writes to a sibling `.tmp` file
//!   and renames over the target, so a kill mid-write never leaves the
//!   restart-reload path with a half-written `state.json`.
//! - **transcript → jsonl**: [`JsonlObserver`] is an
//!   [`crate::selfharness::observer::Observer`] impl that appends every
//!   driver [`Event`](crate::selfharness::observer::Event) as one jsonl
//!   line — reuses the existing pluggable observer seam (WS-H) rather than
//!   adding a second logging path into `driver.rs`.
//!
//! Harness-module reload on restart (the other half of D5) needs no code
//! here: [`crate::selfharness::harness_source::load_harness_source`] always
//! resolves from the on-disk path, and [`crate::selfharness::driver::SelfHarnessDriver::bootstrap`]
//! compiles from that source fresh for a new process — a restarted process
//! constructing a new driver and calling `load_harness_source` again
//! already picks up an edited harness file; only `State` needed an
//! explicit save/restore path.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value as Json;

use super::observer::{Event, Observer};

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("selfharness persistence io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("selfharness persistence: JSON at {path} is malformed: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// Default `State` json path: `<cache_dir>/selfharness/state.json`. A test
/// (or the binary's caller) can point [`crate::selfharness::driver::SelfHarnessDriver::set_state_path`]
/// elsewhere instead — this is only the production default.
pub fn default_state_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("state.json")
}

/// Default transcript jsonl path: `<cache_dir>/selfharness/transcript.jsonl`.
pub fn default_transcript_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("transcript.jsonl")
}

/// Restore the persisted `State` JSON from `path`, if any file exists there
/// yet — `Ok(None)` (NOT an error) when the file is simply absent, which is
/// the expected case for the very first run: [`crate::selfharness::driver::SelfHarnessDriver::run_loop`]
/// then falls back to `initialState` exactly as it does for the in-process
/// very-first-cycle case.
pub fn load_state(path: &Path) -> Result<Option<Json>, PersistenceError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PersistenceError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let value = serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(value))
}

/// Persist `state` to `path`, creating the containing directory if needed.
/// Writes to a `.tmp` sibling then renames over `path` — an atomic
/// replace on the platforms this runs on, so [`load_state`] never observes
/// a partially-written file even if the process is killed mid-write.
pub fn save_state(path: &Path, state: &Json) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bytes = serde_json::to_vec_pretty(state).map_err(|source| PersistenceError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, &bytes).map_err(|source| PersistenceError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| PersistenceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// A transcript [`Observer`]: appends every driver [`Event`] to a jsonl
/// file, one line per event, opened in append mode so a restarted process
/// resumes the same file rather than truncating prior history. Reuses the
/// existing WS-H observer seam ([`crate::selfharness::driver::SelfHarnessDriver`]
/// emits to whatever [`Observer`] it was constructed with) rather than
/// adding a second event-emission path.
pub struct JsonlObserver {
    file: Mutex<std::fs::File>,
    path: PathBuf,
}

impl JsonlObserver {
    /// Open (creating if absent, appending if present) the jsonl transcript
    /// at `path`, creating its containing directory if needed.
    pub fn create(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| PersistenceError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
        })
    }
}

impl Observer for JsonlObserver {
    fn on_event(&self, event: &Event) {
        let line = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[selfharness] transcript: failed to serialize event: {e}");
                return;
            }
        };
        let mut file = match self.file.lock() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[selfharness] transcript: lock poisoned: {e}");
                return;
            }
        };
        if let Err(e) = writeln!(file, "{line}") {
            eprintln!(
                "[selfharness] transcript: write to {} failed: {e}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_state_missing_file_is_none_not_error() {
        let dir = tempfile_dir();
        let path = dir.join("nope").join("state.json");
        assert_eq!(load_state(&path).expect("missing file is Ok(None)"), None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile_dir();
        let path = dir.join("nested").join("state.json");
        let state = serde_json::json!({"loopCount": 3, "mode": "Deciding"});
        save_state(&path, &state).expect("save_state");
        let loaded = load_state(&path).expect("load_state").expect("some state");
        assert_eq!(loaded, state);
    }

    #[test]
    fn save_state_leaves_no_tmp_file_behind() {
        let dir = tempfile_dir();
        let path = dir.join("state.json");
        save_state(&path, &serde_json::json!({"x": 1})).expect("save_state");
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        assert!(!tmp.exists(), "temp file should be renamed away");
        assert!(path.exists());
    }

    #[test]
    fn jsonl_observer_appends_one_line_per_event() {
        let dir = tempfile_dir();
        let path = dir.join("transcript.jsonl");
        let observer = JsonlObserver::create(&path).expect("create");
        observer.on_event(&Event::LoopBoundary);
        observer.on_event(&Event::CompactionTrigger);
        drop(observer);

        let contents = std::fs::read_to_string(&path).expect("read transcript");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("loop_boundary"));
        assert!(lines[1].contains("compaction_trigger"));
    }

    #[test]
    fn jsonl_observer_reopens_in_append_mode_across_restarts() {
        let dir = tempfile_dir();
        let path = dir.join("transcript.jsonl");
        {
            let observer = JsonlObserver::create(&path).expect("create");
            observer.on_event(&Event::LoopBoundary);
        }
        {
            // Simulates a restart: a fresh JsonlObserver over the same path
            // must not truncate the prior line.
            let observer = JsonlObserver::create(&path).expect("re-create");
            observer.on_event(&Event::CompactionTrigger);
        }
        let contents = std::fs::read_to_string(&path).expect("read transcript");
        assert_eq!(contents.lines().count(), 2);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "selfharness-persistence-test-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    static NEXT_TEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
}
