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

/// Append-only, crash-safe, restart-durable.
///
/// Filesystem errors on open/append (missing directory permissions, a full
/// disk) are treated as environment failures and panic with a clear message,
/// the same way [`crate::testing::TestRepo`] treats its own `TempDir`/`fs`
/// setup — [`WorktreeError`] is frozen scaffold and has no variant that fits
/// "the journal file itself could not be written", and inventing one by
/// repurposing an unrelated variant would mislead a caller matching on it.
/// The one recoverable-by-design failure — a torn final row — is handled
/// explicitly below and never reaches a panic.
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
                fs::create_dir_all(parent).expect("create event journal directory");
            }
        }
        // Ensure the file exists so a fresh journal has something to read.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("create event journal file");

        let file = File::open(&path).expect("open event journal file for read");
        let reader = BufReader::new(file);

        let mut entries = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line.expect("read event journal line");
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    // A torn write (crash mid-`writeln!`) leaves an incomplete
                    // final line. Losing that one observation is recoverable;
                    // refusing to open the journal over it is not — so this is
                    // a diagnostic, not a propagated error.
                    eprintln!(
                        "tidepool-worktree: event journal {} line {} unreadable, skipping: {err}",
                        path.display(),
                        idx + 1
                    );
                }
            }
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
            .expect("open event journal file for append");
        writeln!(file, "{line}").expect("write event journal entry");
        file.sync_all().expect("fsync event journal file");

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
