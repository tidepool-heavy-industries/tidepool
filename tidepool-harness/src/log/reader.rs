//! Reads an event-log file written by [`super::LogWriter`]: a from-start
//! iterator (torn-tail tolerant) for replay, and a follow/tail mode for
//! the protocol server's SSE stream.
//!
//! Follow mode polls rather than using a filesystem watcher (`notify` et
//! al.): the contract crate stays dependency-light (see `lib.rs`), the
//! poll loop is a few lines of `std`, and correctness doesn't depend on
//! watcher-backend quirks (inotify coalescing, network-filesystem
//! fallback, etc). A fixed interval is simplest-correct for R0; if SSE
//! latency ever matters a watcher can replace the sleep without changing
//! the `Follower` API.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

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
    /// Reads just the header line. Standalone — does not open or consume
    /// an event iterator, so a version-pin check never disturbs
    /// replay/tail state.
    pub fn read_header(path: impl AsRef<Path>) -> Result<LogHeader, ReadError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(ReadError::Empty);
        }
        Ok(serde_json::from_str(line.trim_end())?)
    }

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

    /// Opens `path`, reads the header, and returns a [`Follower`]
    /// positioned just after it, polling for new events at
    /// `poll_interval`.
    pub fn follow(
        path: impl AsRef<Path>,
        poll_interval: Duration,
    ) -> Result<(LogHeader, Follower), ReadError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(ReadError::Empty);
        }
        let header = serde_json::from_str(line.trim_end())?;
        Ok((
            header,
            Follower {
                reader,
                poll_interval,
                line_buf: String::new(),
            },
        ))
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

/// Tails a log file, blocking (via polling) until the next event lands.
pub struct Follower {
    reader: BufReader<File>,
    poll_interval: Duration,
    line_buf: String,
}

impl Follower {
    /// Blocks until the next event is appended, then returns it. Partial
    /// lines observed mid-poll (writer is between `write_all` and
    /// `sync_all`) accumulate across polls rather than being discarded —
    /// `read_line` appends into `line_buf`, so a line split across polls
    /// is reassembled once its newline lands.
    pub fn next_event(&mut self) -> Result<EventRecord, ReadError> {
        loop {
            self.reader.read_line(&mut self.line_buf)?;
            if self.line_buf.ends_with('\n') {
                let line = std::mem::take(&mut self.line_buf);
                return Ok(serde_json::from_str(line.trim_end())?);
            }
            std::thread::sleep(self.poll_interval);
        }
    }
}
