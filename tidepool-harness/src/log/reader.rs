//! Reads an event-log file written by [`super::LogWriter`]: a from-start
//! iterator (torn-tail tolerant) for replay.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::{EventRecord, LogHeader};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("io error reading event log: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse event log line: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("event log file is empty (missing header)")]
    Empty,
}

/// Entry point for reading a log file written by [`super::LogWriter`].
pub struct LogReader;

impl LogReader {
    /// Opens `path` from the start and returns the header plus an
    /// iterator over its events.
    pub fn open(path: impl AsRef<Path>) -> Result<(LogHeader, EventIter), ReadError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(ReadError::Empty);
        }
        let header = serde_json::from_str(line.trim_end())?;
        Ok((header, EventIter { reader }))
    }
}

/// Yields events from the current file position to EOF. Torn-tail
/// tolerant: a final line left incomplete by a crash mid-append (no
/// trailing newline, or a newline-terminated but truncated JSON body
/// with nothing after it) ends iteration cleanly — the caller sees
/// every whole event and nothing past it, not an error. A parse
/// failure on a line that is NOT the last thing in the file is genuine
/// corruption, not a torn tail, and surfaces as [`ReadError::Parse`].
pub struct EventIter {
    reader: BufReader<File>,
}

impl Iterator for EventIter {
    type Item = Result<EventRecord, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => {
                if !line.ends_with('\n') {
                    // Torn tail: crash left a partial line with no
                    // terminator, and by definition nothing follows it
                    // (read_line only returns without a newline at EOF).
                    // Stop cleanly at the last whole event.
                    return None;
                }
                match serde_json::from_str::<EventRecord>(line.trim_end()) {
                    Ok(record) => Some(Ok(record)),
                    Err(e) => {
                        // A parse failure is only tolerated when this
                        // line is the last thing in the file (a torn
                        // tail that happened to already have its
                        // newline written). If more bytes follow, this
                        // is a corrupt MIDDLE record, not a crash tail —
                        // surface it rather than silently truncating.
                        match self.reader.fill_buf() {
                            Ok([]) => None,
                            Ok(_) => Some(Err(ReadError::Parse(e))),
                            Err(io_err) => Some(Err(ReadError::Io(io_err))),
                        }
                    }
                }
            }
            Err(e) => Some(Err(ReadError::Io(e))),
        }
    }
}
