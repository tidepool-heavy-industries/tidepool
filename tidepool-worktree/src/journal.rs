//! The durable event journal.
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
use serde_json::Value;
use tidepool_repr::jsonl::{self, SyncPolicy};
use tidepool_repr::version_ladder;

use crate::error::WorktreeError;
use crate::id::EventId;
use crate::journal_version::{self, MIGRATIONS};
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

/// Distinguishes the journal's version-stamp header line (`{"version": N}`,
/// no `"cursor"` key) from an ordinary [`JournalEntry`] row (always has
/// `"cursor"`). Only ever checked against the FIRST raw line — see
/// [`EventJournal::open`].
fn is_journal_header(v: &Value) -> bool {
    v.get("cursor").is_none() && v.get("version").is_some()
}

fn ladder_err_to_worktree_err(e: version_ladder::LadderError, path: &Path) -> WorktreeError {
    match e {
        version_ladder::LadderError::BelowFloor { found, floor } => {
            WorktreeError::JournalBelowFloor {
                path: path.to_path_buf(),
                found,
                floor,
            }
        }
        version_ladder::LadderError::UnsupportedVersion { found, current } => {
            WorktreeError::JournalFutureVersion {
                path: path.to_path_buf(),
                found,
                current,
            }
        }
        version_ladder::LadderError::Migration { from, source } => storage_failure(
            path,
            format!("journal migration from version {from} failed: {}", source.0),
        ),
    }
}

impl EventJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| storage_failure(parent, e))?;
            }
        }
        // A fresh journal (no file yet) gets a version-stamped header as
        // its first line, written once at birth — the same discipline
        // `LogWriter::create` uses. Checked BEFORE the `create(true)` open
        // below, which would otherwise erase the "did this file already
        // exist" signal.
        let is_fresh = !path.exists();
        // Ensure the file exists so a fresh journal has something to read.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| storage_failure(&path, e))?;
        if is_fresh {
            let header = serde_json::json!({"version": journal_version::CURRENT}).to_string();
            jsonl::append_new_line(&path, &header, SyncPolicy::All)
                .map_err(|e| storage_failure(&path, e))?;
        }

        // A single-owner file (`&mut self` will never share this path with
        // another handle), so a torn final row is TRUNCATED away — see
        // `tidepool_repr::jsonl`'s module doc for why this is per-consumer.
        // A malformed row anywhere else is loud, matching the old behavior.
        //
        // Parsed as raw `Value` first, not directly into `JournalEntry`:
        // the header line (present in every journal created above; ABSENT
        // in one written before this scheme existed) has a different shape
        // than an entry, and `read_tail`'s single `parse` closure has no
        // way to know which line it's looking at. A shape-level (valid
        // JSON, wrong fields) failure on the true final line therefore no
        // longer benefits from `read_tail`'s own torn-tail forgiveness —
        // only JSON-syntax corruption does — the entry-level loop below
        // restores that forgiveness itself, so the net behavior is
        // unchanged.
        let (raw_lines, torn) = jsonl::read_tail(
            &path,
            |l| serde_json::from_str::<Value>(l).map_err(|e| e.to_string()),
            jsonl::TailPolicy::Repair,
        )
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
        if let Some(repair) = &torn {
            eprintln!(
                "tidepool-worktree: event journal {} line {} is a torn final row, \
                 truncating it away: {}",
                path.display(),
                repair.line_no,
                repair.reason
            );
        }

        // A real header line has no "cursor" key (every `JournalEntry`
        // does) and does carry "version" — see `is_journal_header`. An
        // journal written before this scheme existed has NO header line at
        // all: every line is a plain entry, and the whole file reads as
        // version 0.
        let (found, header_lines, rest): (u32, usize, &[Value]) = match raw_lines.split_first() {
            Some((first, rest)) if is_journal_header(first) => {
                (version_ladder::found_version(first), 1, rest)
            }
            _ => (0, 0, &raw_lines[..]),
        };
        // The header's own bounds must be validated even when the journal
        // has zero entries after it (a header line with nothing else yet
        // appended) — the loop below only runs once there's at least one
        // entry to migrate, so a header-only future-version file needs its
        // own check.
        version_ladder::migrate_to_current(
            Value::Null,
            found,
            journal_version::FLOOR,
            journal_version::CURRENT,
            MIGRATIONS,
        )
        .map_err(|e| ladder_err_to_worktree_err(e, &path))?;

        let n = rest.len();
        let mut entries = Vec::with_capacity(n);
        for (i, raw) in rest.iter().enumerate() {
            let migrated = version_ladder::migrate_to_current(
                raw.clone(),
                found,
                journal_version::FLOOR,
                journal_version::CURRENT,
                MIGRATIONS,
            )
            .map_err(|e| ladder_err_to_worktree_err(e, &path))?;
            match serde_json::from_value::<JournalEntry>(migrated) {
                Ok(entry) => entries.push(entry),
                Err(parse_err) => {
                    let is_final_and_untorn = i == n - 1 && torn.is_none();
                    if is_final_and_untorn {
                        eprintln!(
                            "tidepool-worktree: event journal {} line {} is a torn final row \
                             (shape), leaving it in place: {parse_err}",
                            path.display(),
                            header_lines + i + 1,
                        );
                        break;
                    }
                    return Err(storage_failure(
                        &path,
                        format!(
                            "malformed journal row at line {} is followed by more data — a \
                             corrupted receipt in the middle of the journal is not a torn write \
                             and must not be silently skipped: {parse_err}",
                            header_lines + i + 1,
                        ),
                    ));
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

#[cfg(test)]
mod version_tests {
    use super::*;
    use crate::id::{GitOid, WorktreeId};
    use crate::monitor::{HeadChangeKind, HeadChangeReceipt};

    fn sample_event() -> RepositoryEvent {
        RepositoryEvent::HeadChanged(HeadChangeReceipt {
            worktree: WorktreeId::from_raw("wt-version-test"),
            old_head: None,
            new_head: GitOid::from_raw("a".repeat(40)),
            kind: HeadChangeKind::UnknownChange,
            branch: None,
            observed_at_ms: 1,
        })
    }

    /// A fresh journal's first line is a version-stamped header, and a
    /// reopen never writes a second one.
    #[test]
    fn fresh_journal_gets_exactly_one_header_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        let mut journal = EventJournal::open(&path).unwrap();
        journal.append(&sample_event(), EventId(1)).unwrap();
        drop(journal);
        let mut journal = EventJournal::open(&path).unwrap();
        journal.append(&sample_event(), EventId(2)).unwrap();
        drop(journal);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 3, "header + 2 entries");
        let header: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(
            header,
            serde_json::json!({"version": journal_version::CURRENT})
        );
    }

    /// An unstamped legacy journal (no header line at all — every line a
    /// plain entry) must still load, reading as version 0 and migrating
    /// through the identity step.
    #[test]
    fn unstamped_legacy_journal_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let entry = JournalEntry {
            cursor: 1,
            event_id: EventId(1),
            event: sample_event(),
            recorded_at_ms: 1,
        };
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();

        let journal = EventJournal::open(&path).expect("legacy unstamped journal must load");
        assert_eq!(journal.since(0).len(), 1);
    }

    /// A version newer than this build supports is a loud, typed refusal.
    #[test]
    fn future_version_header_is_a_typed_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "{\"version\":9999}\n").unwrap();

        let err = EventJournal::open(&path).expect_err("a future version must be refused");
        assert!(
            matches!(err, WorktreeError::JournalFutureVersion { found: 9999, .. }),
            "expected JournalFutureVersion, got {err:?}"
        );
    }
}
