//! Local-file persistence for the self-iterating harness: no DB, plain
//! files under `<cache_dir>/selfharness/`.
//!
//! - **[`Checkpoint`]**: the one durable record a restart reads. It carries
//!   a completed cycle's `State` json, the compaction summary in force at
//!   that same cycle, a monotonic `generation`, and a fingerprint of the
//!   harness source that produced it, all written together so a restart can
//!   never pair a state from one cycle with a summary from another.
//!   [`save_checkpoint`]/[`load_checkpoint`] round-trip it through a file,
//!   written atomically (a sibling `.tmp` file, then renamed over the
//!   target) so a kill mid-write never leaves a torn file for
//!   [`load_checkpoint`] to observe.
//! - **transcript → jsonl**: [`JsonlObserver`] is an
//!   [`crate::selfharness::observer::Observer`] impl that appends every
//!   driver [`Event`](crate::selfharness::observer::Event) as one jsonl
//!   line — reuses the existing pluggable observer seam rather than adding
//!   a second logging path into `driver.rs`.
//!
//! Harness-module reload on restart needs no code here:
//! [`crate::selfharness::harness_source::load_harness_source`] always
//! resolves from the on-disk path, and
//! [`crate::selfharness::driver::SelfHarnessDriver::bootstrap`] compiles
//! from that source fresh for a new process — a restarted process
//! constructing a new driver and calling `load_harness_source` again
//! already picks up an edited harness file; only the checkpoint needs an
//! explicit save/restore path.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
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

/// The one durable record a restart reads: a completed cycle's `State`,
/// the compaction summary in force at that same cycle, a monotonic
/// generation counter, a fingerprint of the harness source that produced
/// it, and the loop-iteration count. Written as a whole at one commit
/// boundary — never assembled from two separately-timed writes — so a state
/// and a summary read back together are always from the same generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Monotonic, incremented by one per committed cycle. `0` never appears
    /// on disk — the first commit is generation `1`.
    pub generation: u64,
    /// The loop-boundary `State` json this generation's cycle produced
    /// ([`crate::selfharness::state_cross::state_out`]).
    pub state: Json,
    /// The compaction summary in force when this generation committed —
    /// `None` if no compaction has fired yet at any point up to and
    /// including this cycle.
    pub compaction: Option<String>,
    /// [`crate::selfharness::harness_source::HarnessSource::fingerprint`] of
    /// the source that produced this generation.
    pub harness_source: String,
    /// The number of loop cycles completed as of this generation — a
    /// runtime fact, not part of the authored `State` (see
    /// `plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
    /// context is the runtime's job"). `0` before any cycle has completed;
    /// incremented by one per completed cycle, alongside `generation`.
    pub iteration: u64,
}

/// Default checkpoint path: `<cache_dir>/selfharness/checkpoint.json`. A
/// test (or the binary's caller) can point
/// [`crate::selfharness::driver::SelfHarnessDriver::set_checkpoint_path`]
/// elsewhere instead — this is only the production default.
pub fn default_checkpoint_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("checkpoint.json")
}

/// Default transcript jsonl path: `<cache_dir>/selfharness/transcript.jsonl`.
pub fn default_transcript_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("transcript.jsonl")
}

/// Default DURABLE per-node event-log path:
/// `<cache_dir>/selfharness/log.jsonl`. This is the [`crate::log`] append-only
/// jsonl the answerer [`Harness`](crate::harness::Harness)'s [`LogWriter`](crate::log::LogWriter)
/// writes — `Event::TurnStart { source, .. }` (the executed Haskell of every
/// answerer turn) and `Event::Effect { req, resp, .. }` (drained per turn by
/// `Harness::flush_effects`) for the self-iterating answerer nodes — as opposed
/// to [`default_transcript_path`], which is the loop-level driver-[`Event`]
/// stream. A caller booting the answerer `Harness` points its `LogWriter` here
/// (`LogWriter::create(&default_log_path(), &header)`) so `tail -f` on this one
/// path shows the executed source + effect req/resp interleaved. Sits alongside
/// `checkpoint.json`/`transcript.jsonl` under the same dir.
pub fn default_log_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("log.jsonl")
}

/// Restore the persisted [`Checkpoint`] from `path`, if one exists there
/// yet — `Ok(None)` (NOT an error) when the file is simply absent, which is
/// the expected case for the very first run. A file that exists but fails to
/// parse is a typed [`PersistenceError`], never a silent reset to `None`.
pub fn load_checkpoint(path: &Path) -> Result<Option<Checkpoint>, PersistenceError> {
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
    let checkpoint = serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(checkpoint))
}

/// Persist `checkpoint` to `path`, creating the containing directory if
/// needed. Writes to a `.tmp` sibling then renames over `path` — an atomic
/// replace on the platforms this runs on, so [`load_checkpoint`] never
/// observes a partially-written file even if the process is killed
/// mid-write.
pub fn save_checkpoint(path: &Path, checkpoint: &Checkpoint) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bytes = serde_json::to_vec_pretty(checkpoint).map_err(|source| PersistenceError::Json {
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
/// existing observer seam ([`crate::selfharness::driver::SelfHarnessDriver`]
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

    fn checkpoint(generation: u64) -> Checkpoint {
        Checkpoint {
            generation,
            state: serde_json::json!({"mode": "Deciding"}),
            compaction: Some("a summary".to_string()),
            harness_source: "fingerprint-abc".to_string(),
            iteration: generation,
        }
    }

    #[test]
    fn load_checkpoint_missing_file_is_none_not_error() {
        let dir = tempfile_dir();
        let path = dir.join("nope").join("checkpoint.json");
        assert_eq!(
            load_checkpoint(&path).expect("missing file is Ok(None)"),
            None
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile_dir();
        let path = dir.join("nested").join("checkpoint.json");
        let cp = checkpoint(3);
        save_checkpoint(&path, &cp).expect("save_checkpoint");
        let loaded = load_checkpoint(&path)
            .expect("load_checkpoint")
            .expect("some checkpoint");
        assert_eq!(loaded, cp);
    }

    /// Pure-Rust smoke proof of the restart-continuity requirement
    /// (`plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
    /// context is the runtime's job": the driver "MUST persist the
    /// iteration count in the checkpoint ENVELOPE so restart behavior stays
    /// continuous"), independent of the JIT/GHC-extract-backed acceptance
    /// path ([`crate::selfharness::driver`]'s `SelfHarnessDriver` needs a
    /// compiled harness to run a cycle at all, so this exercises the
    /// envelope logic — [`save_checkpoint`]/[`load_checkpoint`] plus the
    /// `Checkpoint.iteration` field — directly).
    #[test]
    fn iteration_round_trips_through_save_and_load() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");

        // "Cycle 1" commits generation 1 with iteration advanced to 1.
        let cp1 = checkpoint(1);
        assert_eq!(cp1.iteration, 1);
        save_checkpoint(&path, &cp1).expect("save cycle 1");

        // THE point: a bare reload — no cycle run in between — must yield the
        // PERSISTED iteration, not 0. This covers the ENVELOPE half of restart
        // continuity: that `iteration` survives the write/read round trip at
        // all. That the driver then RESUMES from it is a separate claim this
        // test does not cover.
        let restored = load_checkpoint(&path)
            .expect("load after cycle 1")
            .expect("cycle 1's checkpoint is on disk");
        assert_eq!(
            restored.iteration, 1,
            "a restore with no cycle run must resume at the persisted iteration, not reset to 0"
        );

        // "Cycle 2" commits generation 2, continuing the iteration from what
        // was just restored (mirrors `run_one_cycle`'s `self.iteration += 1`
        // after a successful loop, then `commit_checkpoint` persisting it).
        let cp2 = Checkpoint {
            generation: 2,
            iteration: restored.iteration + 1,
            ..checkpoint(2)
        };
        save_checkpoint(&path, &cp2).expect("save cycle 2");
        let restored2 = load_checkpoint(&path)
            .expect("load after cycle 2")
            .expect("cycle 2's checkpoint is on disk");
        assert_eq!(
            restored2.iteration, 2,
            "iteration must continue from the restored value across a second commit, not reset"
        );
    }

    #[test]
    fn save_checkpoint_leaves_no_tmp_file_behind() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        save_checkpoint(&path, &checkpoint(1)).expect("save_checkpoint");
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        assert!(!tmp.exists(), "temp file should be renamed away");
        assert!(path.exists());
    }

    #[test]
    fn truncated_checkpoint_file_is_a_typed_error() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        save_checkpoint(&path, &checkpoint(1)).expect("save_checkpoint");
        let mut bytes = std::fs::read(&path).expect("read back");
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&path, &bytes).expect("write truncated bytes");

        let err = load_checkpoint(&path).expect_err("truncated json must not silently reset");
        assert!(matches!(err, PersistenceError::Json { .. }));
    }

    #[test]
    fn jsonl_observer_appends_one_line_per_event() {
        let dir = tempfile_dir();
        let path = dir.join("transcript.jsonl");
        let observer = JsonlObserver::create(&path).expect("create");
        observer.on_event(&Event::LoopBoundary);
        observer.on_event(&Event::CompactionTrigger {
            node: crate::tree::NodeId(7),
            summary: "distilled work summary".to_string(),
            pre_input_tokens: 900,
            post_input_tokens: 120,
        });
        drop(observer);

        let contents = std::fs::read_to_string(&path).expect("read transcript");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("loop_boundary"));
        assert!(lines[1].contains("compaction_trigger"));
        assert!(lines[1].contains("distilled work summary"));
        assert!(lines[1].contains("900"));
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
            observer.on_event(&Event::CompactionTrigger {
                node: crate::tree::NodeId(1),
                summary: "s".to_string(),
                pre_input_tokens: 1,
                post_input_tokens: 1,
            });
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
