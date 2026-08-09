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

use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::id::EventId;
use crate::monitor::RepositoryEvent;

/// A journalled row. `cursor` is the position AFTER this row — a subscription
/// registering now stores the current end and only ever reads forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    pub cursor: u64,
    pub event_id: EventId,
    pub event: RepositoryEvent,
    pub recorded_at_ms: i64,
}

/// Append-only, crash-safe, restart-durable.
#[derive(Debug)]
pub struct EventJournal {
    path: PathBuf,
}

impl EventJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorktreeError> {
        let _ = path;
        todo!("L3: outside the source tree; a torn final row is skipped, not fatal")
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
        let _ = (event, event_id);
        todo!("L3")
    }

    /// The current end. A fresh subscription starts here — see the module docs.
    pub fn end_cursor(&self) -> u64 {
        todo!("L3")
    }

    /// Rows strictly after `cursor`. For diagnosis and restart recovery only.
    pub fn since(&self, cursor: u64) -> Result<Vec<JournalEntry>, WorktreeError> {
        let _ = cursor;
        todo!("L3")
    }
}
