//! The durable event journal.
//!
//! Every reconciliation is appended here as one atomic observation batch.
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
//! One [`ObservationBatch`] per line (JSONL). A batch owns one `event_id` and
//! every co-emitted view, so a crash can retain all of a reconciliation or
//! none of it, never a misleading prefix.

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
pub struct ObservationBatch {
    pub cursor: u64,
    pub event_id: EventId,
    pub events: Vec<RepositoryEvent>,
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
    entries: Vec<ObservationBatch>,
    write_uncertain: bool,
    /// Exclusive for this handle's lifetime. Cursor and EventId allocation
    /// are derived from the in-memory rows, so a second writer would be stale.
    _owner_lock: fs::File,
}

/// Distinguishes the journal's version-stamp header line (`{"version": N}`,
/// no `"cursor"` key) from an ordinary [`ObservationBatch`] row (always has
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
        let lock_path = path.with_extension("owner.lock");
        let owner_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| storage_failure(&lock_path, e))?;
        match owner_lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(storage_failure(
                    &lock_path,
                    "another EventJournal already owns this path; cursor and EventId allocation require exactly one lifetime-owned writer",
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(storage_failure(&lock_path, e));
            }
        }
        // Initialize an absent OR zero-length journal with one atomic durable
        // header write. Creating the target and appending the header as two
        // operations leaves a crash window in which an empty file reopens as
        // legacy v0 and is permanently below this build's v2 floor. The
        // lifetime lock above makes replacing a zero-length crash remnant safe:
        // no other journal handle can be using this path.
        let needs_header = match fs::metadata(&path) {
            Ok(metadata) => metadata.len() == 0,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(storage_failure(&path, e)),
        };
        if needs_header {
            let header = format!("{{\"version\":{}}}\n", journal_version::CURRENT);
            tidepool_atomic_write::write_durable(&path, header.as_bytes())
                .map_err(|e| storage_failure(&e.path, e.source))?;
        }

        // Open only after initialization has durably published a complete
        // header. Existing nonempty files are never replaced here.
        OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| storage_failure(&path, e))?;

        // This handle owns the file for its lifetime, so a torn final row is
        // TRUNCATED away — see
        // `tidepool_repr::jsonl`'s module doc for why this is per-consumer.
        // A malformed row anywhere else is loud, matching the old behavior.
        //
        // Parsed as raw `Value` first, not directly into `ObservationBatch`:
        // the header line (present in every journal created above; ABSENT
        // in one written before this scheme existed) has a different shape
        // than an entry, and `read_tail`'s single `parse` closure has no
        // way to know which line it's looking at. Torn writes are repaired at
        // the JSON-syntax layer. A complete JSON value with the wrong v2 row
        // shape is corruption and remains loud even at EOF.
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

        // A real header line has no "cursor" key (every `ObservationBatch`
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
            let entry = serde_json::from_value::<ObservationBatch>(migrated).map_err(|e| {
                storage_failure(
                    &path,
                    format!(
                        "malformed journal row shape at line {}: {e}",
                        header_lines + i + 1
                    ),
                )
            })?;
            entries.push(entry);
        }

        Ok(Self {
            path,
            entries,
            write_uncertain: false,
            _owner_lock: owner_lock,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one reconciliation, returning the cursor position after it.
    /// Durable before this returns: an event dispatched but not journalled
    /// would be invisible to the post-mortem that exists to explain it.
    pub fn append(
        &mut self,
        events: &[RepositoryEvent],
        event_id: EventId,
    ) -> Result<u64, WorktreeError> {
        let cursor = self.entries.last().map_or(1, |e| e.cursor + 1);
        let entry = ObservationBatch {
            cursor,
            event_id,
            events: events.to_vec(),
            recorded_at_ms: now_ms(),
        };
        #[allow(clippy::expect_used, reason = "serialize event journal entry")]
        let line = serde_json::to_string(&entry).expect("serialize event journal entry");

        // The shared JSONL primitive performs one write for the complete row
        // plus newline, followed by fsync. The lifetime lock excludes every
        // competing appender, so no stale cursor allocator exists.
        jsonl::append_new_line(&self.path, &line, SyncPolicy::All)
            .map_err(|e| storage_failure(&self.path, e))?;

        self.entries.push(entry);
        Ok(cursor)
    }

    /// The current end. A fresh subscription starts here — see the module docs.
    pub fn end_cursor(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.cursor)
    }

    /// Rows strictly after `cursor`. For diagnosis and restart recovery only.
    /// Infallible — an in-memory filter over already-loaded entries.
    pub fn since(&self, cursor: u64) -> Vec<ObservationBatch> {
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
        journal.append(&[sample_event()], EventId(1)).unwrap();
        drop(journal);
        let mut journal = EventJournal::open(&path).unwrap();
        journal.append(&[sample_event()], EventId(2)).unwrap();
        drop(journal);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 3, "header + 2 entries");
        let header: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(
            header,
            serde_json::json!({"version": journal_version::CURRENT})
        );
    }

    /// A crash between target creation and header publication in the old
    /// initializer left an empty file that was then misclassified as legacy
    /// v0 forever. Empty is an incomplete initialization, not a durable
    /// legacy format, so opening it must atomically install the v2 header.
    #[test]
    fn empty_file_is_reinitialized_with_a_complete_current_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::File::create(&path).unwrap();

        let journal = EventJournal::open(&path).expect("empty initialization remnant recovers");
        assert_eq!(journal.end_cursor(), 0);
        drop(journal);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{{\"version\":{}}}\n", journal_version::CURRENT)
        );
        EventJournal::open(&path).expect("reinitialized journal reopens");
    }

    /// An unstamped event-per-row journal cannot be truthfully reconstructed
    /// into atomic reconciliation batches and is rejected explicitly.
    #[test]
    fn unstamped_legacy_journal_is_below_the_v2_floor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "{\"cursor\":1,\"event_id\":1}\n").unwrap();

        let err = EventJournal::open(&path).expect_err("legacy journal must be rejected");
        assert!(matches!(
            err,
            WorktreeError::JournalBelowFloor {
                found: 0,
                floor: journal_version::FLOOR,
                ..
            }
        ));
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
