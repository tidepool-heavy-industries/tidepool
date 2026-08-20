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

use serde::{Deserialize, Serialize};

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

        let file = File::open(&path).map_err(|e| storage_failure(&path, e))?;
        let mut reader = BufReader::new(file);

        let mut entries = Vec::new();
        // A malformed row is recoverable ONLY as the final row — that is the
        // torn-write shape (a crash mid-`writeln!`). A malformed row with
        // anything after it was not torn by a crash; it is a corrupted receipt,
        // and silently eliding it would delete exactly the evidence the journal
        // exists to preserve. So a bad row is held PENDING and only forgiven at
        // EOF; if any further line arrives, it was not last and we fail loudly.
        //
        // Bytes are counted (hence `read_until`, not `lines()`) because
        // forgiveness must include TAIL REPAIR: `append` opens with O_APPEND,
        // so a torn row merely skipped in memory would get valid rows written
        // AFTER it — manufacturing on disk exactly the corrupted-middle shape
        // this loop refuses, and making the journal permanently unopenable
        // one crash later.
        let mut pending_bad: Option<(usize, String, u64)> = None;
        let not_final = |path: &Path, bad: (usize, String, u64), next: usize| {
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

        let mut buf: Vec<u8> = Vec::new();
        let mut offset: u64 = 0;
        let mut lineno: usize = 0;
        loop {
            buf.clear();
            let n = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| storage_failure(&path, e))?;
            if n == 0 {
                break;
            }
            lineno += 1;
            let line_start = offset;
            offset += n as u64;
            match std::str::from_utf8(&buf) {
                Ok(l) => {
                    if l.trim().is_empty() {
                        if let Some(bad) = pending_bad.take() {
                            return Err(not_final(&path, bad, lineno));
                        }
                        continue;
                    }
                    match serde_json::from_str::<JournalEntry>(l) {
                        Ok(entry) => {
                            if let Some(bad) = pending_bad.take() {
                                return Err(not_final(&path, bad, lineno));
                            }
                            entries.push(entry);
                        }
                        Err(err) => {
                            if let Some(bad) = pending_bad.take() {
                                return Err(not_final(&path, bad, lineno));
                            }
                            pending_bad = Some((lineno, err.to_string(), line_start));
                        }
                    }
                }
                // Non-UTF-8 bytes are the same torn-write shape as a truncated
                // JSON row, so they get the same final-row-only treatment.
                Err(err) => {
                    if let Some(bad) = pending_bad.take() {
                        return Err(not_final(&path, bad, lineno));
                    }
                    pending_bad = Some((lineno, format!("invalid utf-8: {err}"), line_start));
                }
            }
        }

        // Reached EOF with a bad row outstanding: it WAS the final row, so this
        // is the recoverable torn write. Losing that one observation beats
        // refusing to open the journal over it — and the file is TRUNCATED to
        // the last good row so the next `append` lands after good data, not
        // after garbage (see the loop comment).
        if let Some((lineno, reason, bad_start)) = pending_bad {
            eprintln!(
                "tidepool-worktree: event journal {} line {} is a torn final row, \
                 truncating it away: {reason}",
                path.display(),
                lineno
            );
            let repair = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| storage_failure(&path, e))?;
            repair
                .set_len(bad_start)
                .map_err(|e| storage_failure(&path, e))?;
            repair.sync_all().map_err(|e| storage_failure(&path, e))?;
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

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| storage_failure(&self.path, e))?;
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
        // returns.
        let mut row = line;
        row.push('\n');
        file.write_all(row.as_bytes())
            .map_err(|e| storage_failure(&self.path, e))?;
        file.sync_all()
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
    pub fn since(&self, cursor: u64) -> Result<Vec<JournalEntry>, WorktreeError> {
        Ok(self
            .entries
            .iter()
            .filter(|e| e.cursor > cursor)
            .cloned()
            .collect())
    }
}
