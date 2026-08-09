//! The durable event journal — LANE L3.
//!
//! Every reconciled event is appended here with its source, result, timestamp,
//! and [`EventId`].
//!
//! ## The journal is for traceability, NOT replay
//!
//! This is the invariant most likely to be violated by accident, so it is
//! stated as a rule rather than a preference: a subscription registered now
//! begins at the journal's current end and never sees a row written before it.
//! A newly registered handler that replayed history would, in the dev-tree
//! dogfood, poke every child to rebase onto commits they were already built
//! from — an infinite amount of correct-looking, useless work.
//!
//! Restart diagnosis reads the journal. Handlers do not.
//!
//! ## Format
//!
//! One JSON object per line (JSONL), each self-describing its own `cursor` so
//! the file needs no separate index. `append` opens the file, writes one line,
//! fsyncs, and closes — durability holds even if the process dies between two
//! calls, at the cost of a syscall per event, which is the right trade for an
//! event rate driven by git activity rather than a hot loop.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::WorktreeError;
use crate::id::EventId;
use crate::monitor::RepositoryEvent;

/// A journalled row. `cursor` is the position AFTER this row — a subscription
/// registering now stores the current end and only ever reads forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub cursor: u64,
    pub event_id: EventId,
    pub event: RepositoryEvent,
    pub recorded_at_ms: i64,
}

/// Build a [`WorktreeError::StorageFailure`] naming the path that actually
/// failed, from any underlying error with a `Display` impl (`std::io::Error`
/// for I/O, `serde_json::Error` for a corrupt record).
fn storage_failure(path: &Path, detail: impl std::fmt::Display) -> WorktreeError {
    WorktreeError::StorageFailure {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}

/// Append-only, crash-safe, restart-durable.
///
/// Filesystem errors on open/append (missing directory permissions, a full
/// disk) surface as [`WorktreeError::StorageFailure`] naming the journal
/// path, so a resident driving many worktrees can fail the one cycle that hit
/// the fault rather than aborting and taking every other worktree's in-flight
/// work with it. The one recoverable-by-design failure — a torn final row —
/// is handled explicitly below and never reaches an error the caller has to
/// act on.
#[derive(Debug)]
pub struct EventJournal {
    path: PathBuf,
    entries: Vec<JournalEntry>,
}

impl EventJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| storage_failure(parent, e))?;
            }
        }
        // Ensure the file exists so a fresh journal has something to read.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| storage_failure(&path, e))?;

        let file = File::open(&path).map_err(|e| storage_failure(&path, e))?;
        let reader = BufReader::new(file);

        let mut entries = Vec::new();
        // A malformed row is recoverable ONLY as the final row — that is the
        // torn-write shape (a crash mid-`writeln!`). A malformed row with
        // anything after it was not torn by a crash; it is a corrupted receipt,
        // and silently eliding it would delete exactly the evidence the journal
        // exists to preserve. So a bad row is held PENDING and only forgiven at
        // EOF; if any further line arrives, it was not last and we fail loudly.
        let mut pending_bad: Option<(usize, String)> = None;
        let not_final = |path: &Path, bad: (usize, String), next: usize| {
            storage_failure(
                path,
                format!(
                    "malformed journal row at line {} is followed by line {} — a \
                     corrupted receipt in the middle of the journal is not a torn \
                     write and must not be silently skipped: {}",
                    bad.0, next, bad.1
                ),
            )
        };

        for (idx, line) in reader.lines().enumerate() {
            let lineno = idx + 1;
            match line {
                Ok(l) => {
                    if let Some(bad) = pending_bad.take() {
                        return Err(not_final(&path, bad, lineno));
                    }
                    if l.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<JournalEntry>(&l) {
                        Ok(entry) => entries.push(entry),
                        Err(err) => pending_bad = Some((lineno, err.to_string())),
                    }
                }
                // `InvalidData` means `read_line` decoded non-UTF-8 bytes — the
                // same torn-write shape as a truncated JSON row, so it gets the
                // same final-row-only treatment. Any OTHER error (permission
                // denied, a genuine read fault) is not explained by a torn write
                // and propagates immediately.
                Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                    if let Some(bad) = pending_bad.take() {
                        return Err(not_final(&path, bad, lineno));
                    }
                    pending_bad = Some((lineno, format!("invalid utf-8: {err}")));
                }
                Err(err) => return Err(storage_failure(&path, err)),
            }
        }

        // Reached EOF with a bad row outstanding: it WAS the final row, so this
        // is the recoverable torn write. Losing that one observation beats
        // refusing to open the journal over it.
        if let Some((lineno, reason)) = pending_bad {
            eprintln!(
                "tidepool-worktree: event journal {} line {} is a torn final row, skipping: {reason}",
                path.display(),
                lineno
            );
        }

        Ok(Self { path, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event, returning the cursor position after it. Durable before
    /// this returns: an event dispatched to subscribers but not journalled
    /// would be invisible to the post-mortem that exists to explain it.
    pub fn append(
        &mut self,
        event: &RepositoryEvent,
        event_id: EventId,
    ) -> Result<u64, WorktreeError> {
        let cursor = self.entries.last().map_or(1, |e| e.cursor + 1);
        let entry = JournalEntry {
            cursor,
            event_id,
            event: event.clone(),
            recorded_at_ms: now_ms(),
        };
        let line = serde_json::to_string(&entry).expect("serialize event journal entry");

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| storage_failure(&self.path, e))?;
        writeln!(file, "{line}").map_err(|e| storage_failure(&self.path, e))?;
        file.sync_all()
            .map_err(|e| storage_failure(&self.path, e))?;

        self.entries.push(entry);
        Ok(cursor)
    }

    /// The current end. A fresh subscription starts here — see the module docs.
    pub fn end_cursor(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.cursor)
    }

    /// Rows strictly after `cursor`. For diagnosis and restart recovery only.
    pub fn since(&self, cursor: u64) -> Result<Vec<JournalEntry>, WorktreeError> {
        Ok(self
            .entries
            .iter()
            .filter(|e| e.cursor > cursor)
            .cloned()
            .collect())
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}
