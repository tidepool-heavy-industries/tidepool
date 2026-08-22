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

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tidepool_repr::jsonl::{self, SyncPolicy};

use crate::error::WorktreeError;
use crate::id::EventId;
use crate::monitor::RepositoryEvent;
use crate::storage::{now_ms, storage_failure};

/// A journalled row. `cursor` is the position AFTER this row — a subscription
/// registering now stores the current end and only ever reads forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub cursor: u64,
    pub event_id: EventId,
    pub event: RepositoryEvent,
    pub recorded_at_ms: i64,
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

        // Tail repair (a torn final row truncated away) is unconditional in
        // the shared reader — see `tidepool_repr::jsonl`'s module doc. A
        // malformed row anywhere else is loud, matching the old behavior.
        let (entries, repair) = jsonl::read_repairing_tail(&path, |l| {
            serde_json::from_str::<JournalEntry>(l).map_err(|e| e.to_string())
        })
        .map_err(|e| match e {
            jsonl::JsonlReadError::Io(io) => storage_failure(&path, io),
            jsonl::JsonlReadError::TornMidFile { line_no, detail } => storage_failure(
                &path,
                format!(
                    "malformed journal row at line {line_no} is followed by more data — a \
                     corrupted receipt in the middle of the journal is not a torn write and \
                     must not be silently skipped: {detail}"
                ),
            ),
        })?;
        if let Some(repair) = repair {
            eprintln!(
                "tidepool-worktree: event journal {} line {} is a torn final row, \
                 truncating it away: {}",
                path.display(),
                repair.line_no,
                repair.reason
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
        #[allow(clippy::expect_used, reason = "serialize event journal entry")]
        let line = serde_json::to_string(&entry).expect("serialize event journal entry");

        // ONE write() for the whole row (line + trailing newline), not
        // `writeln!`'s two syscalls — with O_APPEND, Linux serializes each
        // write() under the inode lock (seek-to-end + write happen as one
        // step), so two processes/threads appending to this journal can
        // never land their bytes interleaved. POSIX itself only guarantees
        // append-write atomicity up to PIPE_BUF-ish sizes; Linux (this
        // crate's deployment target) does not impose that cap in practice,
        // but if a single `JournalEntry` line ever grows well past a few KB
        // (e.g. a huge `files` list on a `Commit` event), that's outside
        // what this has been verified against and the interleave risk
        // returns. No lock is needed here: `&mut self` is already exclusive.
        jsonl::append_new_line(&self.path, &line, SyncPolicy::All)
            .map_err(|e| storage_failure(&self.path, e))?;

        self.entries.push(entry);
        Ok(cursor)
    }

    /// The current end. A fresh subscription starts here — see the module docs.
    pub fn end_cursor(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.cursor)
    }

    /// Every journalled entry, oldest first. The monitor's retry-idempotency
    /// check reads this to recognize an observation it already recorded.
    pub fn iter(&self) -> std::slice::Iter<'_, JournalEntry> {
        self.entries.iter()
    }

    /// Rows strictly after `cursor`. For diagnosis and restart recovery only.
    /// Infallible — an in-memory filter over already-loaded entries.
    pub fn since(&self, cursor: u64) -> Vec<JournalEntry> {
        self.entries
            .iter()
            .filter(|e| e.cursor > cursor)
            .cloned()
            .collect()
    }
}
