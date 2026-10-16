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
//! every co-emitted view. A partial final JSON row is repaired on exclusive
//! reopen; a failed append may already be visible and poisons its writer.

use std::fs::{self, OpenOptions};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tidepool_repr::jsonl::{self, SyncPolicy};

use crate::error::WorktreeError;
use crate::id::EventId;
use crate::journal_version;
use crate::monitor::RepositoryEvent;
use crate::storage::{now_ms, storage_failure};
use tidepool_atomic_write::DirectoryAnchor;

/// A journalled row. `cursor` is the position AFTER this row — a subscription
/// registering now stores the current end and only ever reads forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationBatch {
    pub cursor: u64,
    pub event_id: EventId,
    pub events: Vec<RepositoryEvent>,
    pub recorded_at_ms: i64,
}

/// Append-only, with acknowledged rows synced before success.
///
/// A failed append leaves this handle uncertain: further writes are refused
/// until it is dropped and exclusively reopened. Diagnostic reads retain the
/// last acknowledged snapshot, not a claim about possibly visible failed writes.
/// Reopen reconciles retained rows and the explicitly repairable torn final row.
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

fn check_version(found: u32, path: &Path) -> Result<(), WorktreeError> {
    if found < journal_version::CURRENT {
        return Err(WorktreeError::JournalBelowFloor {
            path: path.to_path_buf(),
            found,
            floor: journal_version::CURRENT,
        });
    }
    if found > journal_version::CURRENT {
        return Err(WorktreeError::JournalFutureVersion {
            path: path.to_path_buf(),
            found,
            current: journal_version::CURRENT,
        });
    }
    Ok(())
}

#[derive(Deserialize)]
struct JournalHeader {
    version: u32,
}

impl EventJournal {
    /// Open a journal below an established storage boundary, confirming its
    /// parent hierarchy before admitting the exclusive writer.
    pub fn open(
        anchor: &DirectoryAnchor,
        relative: impl AsRef<Path>,
    ) -> Result<Self, WorktreeError> {
        let relative = relative.as_ref();
        let path = anchor
            .resolve(relative)
            .map_err(|e| storage_failure(&e.path, e.source))?;
        if relative.file_name().is_none() {
            return Err(storage_failure(
                relative,
                "journal path requires a filename",
            ));
        }
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        anchor
            .create_dir_all(parent)
            .map_err(|e| storage_failure(&e.path, e.source))?;
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
        // header write. An empty file is an incomplete initialization. The
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
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| storage_failure(&path, e))?;

        // Reject unsupported headers before touching a possibly torn row tail.
        // The first nonempty row owns the format; ordinary batches have no header.
        let first_line =
            std::io::BufReader::new(fs::File::open(&path).map_err(|e| storage_failure(&path, e))?)
                .lines()
                .find_map(|line| match line {
                    Ok(line) if line.trim().is_empty() => None,
                    line => Some(line),
                })
                .transpose()
                .map_err(|e| storage_failure(&path, e))?;
        let first: Value = serde_json::from_str(first_line.as_deref().unwrap_or("null"))
            .map_err(|e| storage_failure(&path, e))?;
        if !is_journal_header(&first) {
            check_version(0, &path)?;
        }
        let header: JournalHeader =
            serde_json::from_value(first).map_err(|e| storage_failure(&path, e))?;
        check_version(header.version, &path)?;

        // Only incomplete JSON tail syntax is repairable. Complete rows with
        // invalid batch schemas are refused without truncation.
        let (raw_lines, torn) = jsonl::read_tail(
            &path,
            |l| serde_json::from_str::<Value>(l).map_err(|e| e.to_string()),
            jsonl::TailPolicy::Repair,
        )
        .map_err(|e| match e {
            jsonl::JsonlReadError::Io(io) => storage_failure(&path, io),
            jsonl::JsonlReadError::MalformedRow { line_no, detail } => storage_failure(
                &path,
                format!("malformed journal row at line {line_no}: {detail}"),
            ),
        })?;
        if let Some(repair) = &torn {
            eprintln!(
                "exomonad-worktree: event journal {} line {} is a torn final row, \
                 truncating it away: {}",
                path.display(),
                repair.line_no,
                repair.reason
            );
        }

        let mut rows = raw_lines.into_iter();
        rows.next(); // The header was validated before tail repair.
        let mut entries = Vec::with_capacity(rows.len());
        for (i, raw) in rows.enumerate() {
            let entry = serde_json::from_value::<ObservationBatch>(raw).map_err(|e| {
                storage_failure(
                    &path,
                    format!("malformed journal row shape at line {}: {e}", i + 2),
                )
            })?;
            entries.push(entry);
        }

        // A prior writer may have failed after making bytes visible. Confirm
        // the reconciled contents and pathname before admitting another writer.
        file.sync_all().map_err(|e| storage_failure(&path, e))?;
        tidepool_atomic_write::sync_parent_directory(&path)
            .map_err(|e| storage_failure(&e.path, e.source))?;

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
        self.ensure_writable()?;
        let cursor = self
            .end_cursor()
            .checked_add(1)
            .ok_or_else(|| storage_failure(&self.path, "journal cursor exhausted"))?;
        let entry = ObservationBatch {
            cursor,
            event_id,
            events: events.to_vec(),
            recorded_at_ms: now_ms(),
        };
        #[allow(clippy::expect_used, reason = "serialize event journal entry")]
        let line = serde_json::to_string(&entry).expect("serialize event journal entry");

        // The lifetime lock excludes competing appenders, but an I/O error
        // may follow a partial or complete visible row. Never reuse its cursor.
        if let Err(error) = jsonl::append_new_line(&self.path, &line, SyncPolicy::All) {
            self.write_uncertain = true;
            return Err(storage_failure(&self.path, error));
        }

        self.entries.push(entry);
        Ok(cursor)
    }

    /// Refuse mutations based on a snapshot whose last publication is uncertain.
    pub(crate) fn ensure_writable(&self) -> Result<(), WorktreeError> {
        if self.write_uncertain {
            return Err(storage_failure(&self.path,
                "journal append outcome is uncertain; drop and exclusively reopen before further mutation"));
        }
        Ok(())
    }

    /// The last acknowledged end, not possibly visible failed appends.
    /// A fresh subscription starts here — see the module docs.
    pub fn end_cursor(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.cursor)
    }

    /// Borrow acknowledged rows for diagnosis and restart recovery.
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &ObservationBatch> {
        self.entries.iter()
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
#[path = "journal_properties.rs"]
mod properties;

#[cfg(test)]
mod version_tests {
    use super::*;
    use crate::id::{GitOid, WorktreeId};
    use crate::monitor::{HeadChangeKind, HeadChangeReceipt};

    #[test]
    fn missing_filename_cannot_create_an_owner_lock_outside_the_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(dir.path()).unwrap();
        let lock = anchor.path().with_extension("owner.lock");
        assert!(!lock.exists());
        for relative in ["", "."] {
            assert!(EventJournal::open(&anchor, relative).is_err());
        }
        assert!(!lock.exists());
        assert_eq!(std::fs::read_dir(anchor.path()).unwrap().count(), 0);
    }

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
        let dir_anchor = DirectoryAnchor::open_existing(dir.path()).unwrap();
        let path = dir.path().join("events.jsonl");

        let mut journal = EventJournal::open(&dir_anchor, "events.jsonl").unwrap();
        journal.append(&[sample_event()], EventId(1)).unwrap();
        drop(journal);
        let mut journal = EventJournal::open(&dir_anchor, "events.jsonl").unwrap();
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

    /// Empty initialization remnants atomically acquire a current header.
    #[test]
    fn empty_file_is_reinitialized_with_a_complete_current_header() {
        let dir = tempfile::tempdir().unwrap();
        let dir_anchor = DirectoryAnchor::open_existing(dir.path()).unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::File::create(&path).unwrap();

        let journal = EventJournal::open(&dir_anchor, "events.jsonl")
            .expect("empty initialization remnant recovers");
        assert_eq!(journal.end_cursor(), 0);
        drop(journal);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{{\"version\":{}}}\n", journal_version::CURRENT)
        );
        EventJournal::open(&dir_anchor, "events.jsonl").expect("reinitialized journal reopens");
    }

    #[test]
    fn unsupported_headers_refuse_before_tail_repair_and_retain_typed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(dir.path()).unwrap();
        let path = dir.path().join("events.jsonl");
        for (header, found) in [
            ("{\"cursor\":1,\"event_id\":1}", 0),
            ("{\"version\":1}", 1),
            ("{\"version\":9999}", 9999),
        ] {
            let bytes = format!("{header}\n{{\"cursor\":").into_bytes();
            std::fs::write(&path, &bytes).unwrap();
            let error = EventJournal::open(&anchor, "events.jsonl").unwrap_err();
            match error {
                WorktreeError::JournalBelowFloor {
                    path: actual,
                    found: actual_found,
                    floor: 2,
                } if found < 2 => {
                    assert_eq!(actual, path);
                    assert_eq!(actual_found, found);
                }
                WorktreeError::JournalFutureVersion {
                    path: actual,
                    found: actual_found,
                    current: 2,
                } if found > 2 => {
                    assert_eq!(actual, path);
                    assert_eq!(actual_found, found);
                }
                error => panic!("unexpected version rejection: {error:?}"),
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        // The accepted format still repairs an incomplete tail.
        std::fs::write(&path, b"{\"version\":2}\n{\"cursor\":").unwrap();
        let journal = EventJournal::open(&anchor, "events.jsonl").unwrap();
        assert_eq!(journal.end_cursor(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"version\":2}\n");
    }
}
