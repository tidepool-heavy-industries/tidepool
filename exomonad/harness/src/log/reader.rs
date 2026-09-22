//! Reads an event-log file written by [`super::LogWriter`]: the whole file
//! is read (torn-tail tolerant) via the shared
//! [`tidepool_repr::jsonl::read_tail`] primitive, version-gated and
//! migrated against `super::version`'s ladder, then decoded into a
//! [`LogHeader`] plus a `Vec<EventRecord>` — eager, not the previous
//! hand-rolled `BufReader` streaming reader, since a version stamp is a
//! whole-file property (every event's shape depends on the header's own
//! declared version) that can only be resolved once the header line is in
//! hand, and the shared primitive already reads the whole file up front to
//! do that. `fold_tree_state`/`ReplayProvider::from_log` (this reader's
//! only two callers) always drain the iterator to completion regardless.

use std::path::Path;

use serde_json::Value;
use tidepool_repr::jsonl::{self, TailPolicy};
use tidepool_repr::version_ladder::{self, LadderError};

use super::version::{EVENT_MIGRATIONS, HEADER_MIGRATIONS};
use super::{EventRecord, LogHeader};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("io error reading event log: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse event log line: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("event log file is empty (missing header)")]
    Empty,
    #[error("event log's first line is not a header")]
    MissingHeader,
    #[error(
        "malformed row at line {line_no} is followed by more data — not a torn write, and must \
         not be silently skipped: {detail}"
    )]
    TornMidFile { line_no: usize, detail: String },
    /// Below the floor this build still carries a migration path from —
    /// never a silent reset. See `persistence-versioning-design.md` §6.
    #[error(
        "event log version {found} is below the floor this build still supports ({floor}) — \
         archive or delete the log and start a fresh run, or read it with an older tidepool \
         build that still supports version {found}"
    )]
    BelowFloor { found: u32, floor: u32 },
    /// Newer than this build knows how to read.
    #[error(
        "event log version {found} is newer than this build supports (current {current}) — \
         rebuild against a newer tidepool, or archive/delete the log and start a fresh run"
    )]
    FutureVersion { found: u32, current: u32 },
    #[error("event log migration failed: {0}")]
    Migration(String),
}

impl From<jsonl::JsonlReadError> for ReadError {
    fn from(e: jsonl::JsonlReadError) -> Self {
        match e {
            jsonl::JsonlReadError::Io(io) => ReadError::Io(io),
            jsonl::JsonlReadError::TornMidFile { line_no, detail } => {
                ReadError::TornMidFile { line_no, detail }
            }
        }
    }
}

impl From<LadderError> for ReadError {
    fn from(e: LadderError) -> Self {
        match e {
            LadderError::BelowFloor { found, floor } => ReadError::BelowFloor { found, floor },
            LadderError::UnsupportedVersion { found, current } => {
                ReadError::FutureVersion { found, current }
            }
            LadderError::Migration { from, source } => {
                ReadError::Migration(format!("from version {from}: {source}"))
            }
        }
    }
}

/// Entry point for reading a log file written by [`super::LogWriter`].
pub struct LogReader;

impl LogReader {
    /// Reads `path` in full and returns the header plus an iterator over its
    /// events. Torn-tail tolerant: a final line left incomplete by a crash
    /// mid-append is dropped, never surfaced as an error — see
    /// [`tidepool_repr::jsonl::read_tail`]'s module doc. A parse failure on a
    /// line that is NOT the last thing in the file is genuine corruption,
    /// not a torn tail, and surfaces as [`ReadError::TornMidFile`].
    pub fn open(path: impl AsRef<Path>) -> Result<(LogHeader, EventIter), ReadError> {
        // `Observe`, not `Repair`: this reader has never mutated the log
        // file on disk (the old hand-rolled reader only ever stopped
        // cleanly at a torn tail in memory), and a durable per-run log is
        // exactly the kind of file worth leaving untouched for later
        // forensic reading.
        let (raw_lines, _torn) = jsonl::read_tail(
            path.as_ref(),
            |l| serde_json::from_str::<Value>(l).map_err(|e| e.to_string()),
            TailPolicy::Observe,
        )?;

        let mut lines = raw_lines.into_iter();
        let first = lines.next().ok_or(ReadError::Empty)?;
        // An `EventRecord` line always carries a top-level "event" key
        // (`{"seq": N, "event": {...}}`); `LogHeader` never does — see
        // `super::mod`'s doc on the envelope shape.
        if first.get("event").is_some() {
            return Err(ReadError::MissingHeader);
        }
        let found = version_ladder::found_version(&first);
        let header_value = version_ladder::migrate_to_current(
            first,
            found,
            super::version::FLOOR,
            super::version::CURRENT,
            HEADER_MIGRATIONS,
        )?;
        let header: LogHeader = serde_json::from_value(header_value)?;

        let mut records = Vec::new();
        for raw in lines {
            let migrated = version_ladder::migrate_to_current(
                raw,
                found,
                super::version::FLOOR,
                super::version::CURRENT,
                EVENT_MIGRATIONS,
            )?;
            let record: EventRecord = serde_json::from_value(migrated)?;
            records.push(Ok(record));
        }

        Ok((
            header,
            EventIter {
                inner: records.into_iter(),
            },
        ))
    }
}

/// Yields every whole event in a log file, in `seq` order. See
/// [`LogReader::open`]'s doc for the torn-tail/corruption discipline.
#[derive(Debug)]
pub struct EventIter {
    inner: std::vec::IntoIter<Result<EventRecord, ReadError>>,
}

impl Iterator for EventIter {
    type Item = Result<EventRecord, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}
