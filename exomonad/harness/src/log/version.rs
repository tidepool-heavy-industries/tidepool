//! Version stamp for the harness log wire contract, one of the durable
//! artifact kinds `tidepool_repr::version_ladder` covers (see that crate's
//! CLAUDE.md for the mechanism). One counter governs both [`super::LogHeader`]'s own shape and
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

/// This build's current harness-log version.
pub const CURRENT: u32 = 2;
/// The v2 schema deliberately drops the retired snapshot/branch receipt
/// variants. Older logs are rejected explicitly rather than partially read.
pub const FLOOR: u32 = 2;

/// Indexed from [`FLOOR`]: `HEADER_MIGRATIONS[0]` is the `FLOOR -> FLOOR+1`
/// step for [`super::LogHeader`]'s own shape.
pub const HEADER_MIGRATIONS: &[tidepool_repr::version_ladder::Migration] = &[];
/// Indexed from [`FLOOR`]: `EVENT_MIGRATIONS[0]` is the `FLOOR -> FLOOR+1`
/// step for an [`super::EventRecord`] row's shape.
pub const EVENT_MIGRATIONS: &[tidepool_repr::version_ladder::Migration] = &[];
