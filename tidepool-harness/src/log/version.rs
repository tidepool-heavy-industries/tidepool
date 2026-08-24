//! Version stamp for the harness log wire contract (kind 3 of
//! `plans/persistence-versioning-design.md`'s six persistence-versioning
//! kinds). One counter governs both [`super::LogHeader`]'s own shape and
//! every [`super::EventRecord`] line in the file — the header is the file's
//! one declaration of "every event below is at this version."
//!
//! `LogHeader` itself carries no `version` field (deliberately — see
//! [`super::writer::StampedHeader`]'s doc: a bare struct field would force
//! every one of this workspace's ~80 `LogHeader { .. }` literal
//! construction sites, almost all test fixtures, to learn about
//! versioning). The version lives in the WIRE ENVELOPE only, added by
//! [`super::writer::LogWriter::create`] and stripped by
//! [`super::reader::LogReader::open`].

use serde_json::Value;
use tidepool_repr::version_ladder::{set_version, Migration, MigrationError};

/// This build's current harness-log version.
pub const CURRENT: u32 = 1;
/// The oldest version this build still loads. `0` — an unstamped log
/// written before this scheme existed — stays accepted so an existing
/// operator's `log-*.jsonl` keeps loading; see `persistence-versioning-design.md`
/// §6 for when this is ever raised.
pub const FLOOR: u32 = 0;

fn header_v0_to_v1(v: Value) -> Result<Value, MigrationError> {
    // Purely additive: `0` and `1` are the same `LogHeader` shape, this
    // step only makes the version explicit. See the module doc's
    // unstamped-file convention.
    Ok(set_version(v, 1))
}

fn event_v0_to_v1(v: Value) -> Result<Value, MigrationError> {
    // Same story as `header_v0_to_v1`, for `EventRecord`'s shape.
    Ok(set_version(v, 1))
}

/// Indexed from [`FLOOR`]: `HEADER_MIGRATIONS[0]` is the `FLOOR -> FLOOR+1`
/// step for [`super::LogHeader`]'s own shape.
pub const HEADER_MIGRATIONS: &[Migration] = &[header_v0_to_v1];
/// Indexed from [`FLOOR`]: `EVENT_MIGRATIONS[0]` is the `FLOOR -> FLOOR+1`
/// step for an [`super::EventRecord`] row's shape.
pub const EVENT_MIGRATIONS: &[Migration] = &[event_v0_to_v1];
