//! Version stamp for [`super::journal::JournalHandler`]'s per-segment wire
//! contract — kind 5 of `plans/persistence-versioning-design.md`'s six
//! persistence-versioning kinds. Stamped PER SEGMENT, not per run: PRD 20's
//! segmented-journal design already treats each segment file as
//! independently readable/foldable (`tidepool_harness::selfharness::resume`),
//! and a run-level version would need to reach into that crate's
//! lease/fold machinery, which sits above this crate in the dependency
//! graph — see `persistence-versioning-design.md` §2/Open Question 4.

use serde_json::Value;
use tidepool_repr::version_ladder::{set_version, Migration, MigrationError};

/// This build's current per-segment journal version.
pub const CURRENT: u32 = 1;
/// The oldest version this build still loads. `0` — a segment written
/// before this scheme existed, carrying no header line at all — stays
/// accepted so an existing run's journal segments keep loading.
pub const FLOOR: u32 = 0;

fn entry_v0_to_v1(v: Value) -> Result<Value, MigrationError> {
    // Purely additive: `0` and `1` are the same `JournalEntry` shape, this
    // step only makes the version explicit.
    Ok(set_version(v, 1))
}

/// Indexed from [`FLOOR`].
pub const MIGRATIONS: &[Migration] = &[entry_v0_to_v1];
