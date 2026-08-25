//! Version stamp for [`super::journal::EventJournal`]'s wire contract, on
//! `tidepool_repr::version_ladder` (the one migration-ladder mechanism for
//! durable, non-reproducible persistence artifacts). `EventJournal` is
//! single-owner (`&mut self` exclusive, see `journal.rs`'s own doc), so the
//! header is written once, at file creation, the same discipline
//! `LogWriter::create` uses.

use serde_json::Value;
use tidepool_repr::version_ladder::{set_version, Migration, MigrationError};

/// This build's current worktree-journal version.
pub const CURRENT: u32 = 1;
/// The oldest version this build still loads. `0` — a journal written
/// before this scheme existed, carrying no header line at all — stays
/// accepted so an existing worktree's `events.jsonl` keeps loading.
pub const FLOOR: u32 = 0;

fn entry_v0_to_v1(v: Value) -> Result<Value, MigrationError> {
    // Purely additive: `0` and `1` are the same `JournalEntry` shape, this
    // step only makes the version explicit.
    Ok(set_version(v, 1))
}

/// Indexed from [`FLOOR`].
pub const MIGRATIONS: &[Migration] = &[entry_v0_to_v1];
