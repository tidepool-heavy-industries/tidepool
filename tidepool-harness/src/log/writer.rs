//! Append-only jsonl writer: header first line, one [`Event`] per line
//! after, fsync'd on every append so a crash never loses an acknowledged
//! event.

use std::fs::{File, OpenOptions};
use std::path::Path;

use serde::Serialize;
use tidepool_repr::jsonl::{self, SyncPolicy};

use super::version::CURRENT as LOG_VERSION_CURRENT;
use super::{Event, EventRecord, LogHeader};

/// The wire envelope [`LogWriter::create`] actually writes as line one: the
/// version stamp plus every [`LogHeader`] field flattened alongside it.
/// Keeping this OUT of `LogHeader` itself is deliberate — see
/// `super::version`'s module doc for why a bare struct field would ripple
/// into ~80 call sites across this workspace that build a `LogHeader`
/// literal with no reason to know about versioning.
#[derive(Serialize)]
struct StampedHeader<'a> {
    version: u32,
    #[serde(flatten)]
    header: &'a LogHeader,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("io error writing event log: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to serialize event log line: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Owns one run's log file. `next_seq` is the writer-assigned, monotonic,
/// per-file sequence counter for the [`EventRecord`] envelope.
pub struct LogWriter {
    file: File,
    next_seq: u64,
}

impl LogWriter {
    /// Creates a new run file at `path` (error if one already exists —
    /// a run's log is never silently overwritten) and writes the header
    /// as the first line.
    pub fn create(path: impl AsRef<Path>, header: &LogHeader) -> Result<Self, WriteError> {
        let file = OpenOptions::new().create_new(true).write(true).open(path)?;
        let mut writer = LogWriter { file, next_seq: 0 };
        writer.write_line(&StampedHeader {
            version: LOG_VERSION_CURRENT,
            header,
        })?;
        Ok(writer)
    }

    /// Appends `event`, assigning it the next per-file `seq`, and fsyncs
    /// before returning. Returns the assigned `seq`.
    pub fn append(&mut self, event: Event) -> Result<u64, WriteError> {
        let seq = self.next_seq;
        let record = EventRecord { seq, event };
        self.write_line(&record)?;
        self.next_seq += 1;
        Ok(seq)
    }

    fn write_line<T: serde::Serialize>(&mut self, value: &T) -> Result<(), WriteError> {
        let line = serde_json::to_string(value)?;
        jsonl::write_line(&mut self.file, &line, SyncPolicy::All)?;
        Ok(())
    }
}
