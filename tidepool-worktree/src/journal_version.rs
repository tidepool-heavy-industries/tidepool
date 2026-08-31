//! Version stamp for [`super::journal::EventJournal`]'s wire contract, on
//! `tidepool_repr::version_ladder` (the one migration-ladder mechanism for
//! durable, non-reproducible persistence artifacts). `EventJournal` is
//! single-owner (enforced with a lifetime lock; see `journal.rs`), so the
//! header is written once at file creation.

use tidepool_repr::version_ladder::Migration;

/// This build's current worktree-journal version.
pub const CURRENT: u32 = 2;
/// v2 deliberately drops event-per-row journals. A v0/v1 file can contain a
/// partially appended reconciliation, so no migration can honestly infer its
/// missing batch boundary or restore the shared EventId invariant.
pub const FLOOR: u32 = 2;

/// Indexed from [`FLOOR`].
pub const MIGRATIONS: &[Migration] = &[];
