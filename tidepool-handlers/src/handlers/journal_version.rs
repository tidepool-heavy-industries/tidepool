//! Version stamp for [`super::journal::JournalHandler`]'s per-segment wire
//! contract, on `tidepool_repr::version_ladder` (the one migration-ladder
//! mechanism for durable, non-reproducible persistence artifacts). Stamped
//! PER SEGMENT, not per run: the segmented-journal design already treats
//! each segment file as independently readable/foldable
//! (`tidepool_harness::selfharness::resume`), and a run-level version would
//! need to reach into that crate's lease/fold machinery, which sits above
//! this crate in the dependency graph.

use serde_json::Value;
use tidepool_repr::version_ladder::{set_version, Migration, MigrationError};

/// This build's current per-segment journal version.
pub const CURRENT: u32 = 2;
/// The oldest version this build still loads. `0` — a segment written
/// before this scheme existed, carrying no header line at all — stays
/// accepted so an existing run's journal segments keep loading.
pub const FLOOR: u32 = 0;

fn entry_v0_to_v1(v: Value) -> Result<Value, MigrationError> {
    // Purely additive: `0` and `1` are the same `JournalEntry` shape, this
    // step only makes the version explicit.
    Ok(set_version(v, 1))
}

fn entry_v1_to_v2(mut v: Value) -> Result<Value, MigrationError> {
    // Entries written before provenance timestamps existed have no honest
    // wall-clock value to recover. `0` is the documented "unknown" sentinel;
    // current decoding uses the same default so even raw legacy rows remain
    // backwards compatible outside the segment loader.
    if let Value::Object(map) = &mut v {
        map.entry("ts".to_string())
            .or_insert_with(|| Value::from(0));
    }
    Ok(set_version(v, 2))
}

/// Indexed from [`FLOOR`].
pub const MIGRATIONS: &[Migration] = &[entry_v0_to_v1, entry_v1_to_v2];
