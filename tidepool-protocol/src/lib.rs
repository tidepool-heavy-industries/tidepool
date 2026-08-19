//! The Tidepool effect protocol, as data.
//!
//! One schema describing effects, verbs, records, errors, field names and
//! types — plus the generators that project it into the crates that consume it.
//! NOT a network IDL, and no runtime component: nothing here is linked into the
//! server.
//!
//! # Why this crate exists
//!
//! The effect contract used to be single-sourced per SLICE rather than as a
//! protocol: one verb's truth was spread across up to four hand-maintained
//! registries — the macro DSL, the wire structs whose field ORDER was
//! maintained by comment, the extractor's per-verb tables, and the harness's
//! constructor-NAME classification lists. A verb added to one and missed in
//! another did not fail; it MISROUTED. This crate is where a verb's truth lives
//! instead.
//!
//! # The two rules that keep it honest
//!
//! **It is a leaf.** Zero dependencies, `std` only, never on another tidepool
//! crate. The crates it describes consume its OUTPUT, so a dependency in the
//! other direction would make the schema unusable from the low crates.
//!
//! **No raw escape hatches.** There is no "arbitrary Haskell source" slot and
//! no "arbitrary Rust body" slot, because that is precisely how a schema stops
//! being authoritative. Helper shapes are parameterized, reviewed patterns
//! ([`schema::HelperBody`]); types come from a closed language ([`hs::HsType`]).
//! Anything that cannot be expressed either becomes a deliberate schema feature
//! or stays hand-written OUTSIDE the contract — it is never smuggled in as a
//! string.
//!
//! # Migration status
//!
//! Effects move here one at a time, each proven byte-compatible before its
//! hand-written copy is deleted. [`effects::all`] is the migrated set;
//! everything else still lives in `tidepool-mcp/src/effect_defs.rs`. See
//! `plans/self-iterating-harness/22-p1-protocol-scaffold.md` for the schema
//! design, the golden protocol, and the procedure for migrating the next one.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod effects;
pub mod gen;
pub mod hs;
pub mod schema;
pub mod types;

pub use gen::{all_files, GeneratedFile};
pub use hs::HsType;
pub use schema::Effect;

/// Every generated file for every migrated effect, in a stable order.
///
/// # Panics
/// Panics when a migrated effect fails [`schema::Effect::validate`]. A
/// malformed schema must never produce output — the failure belongs at
/// generation time, which is the whole point of the annotations being required
/// fields.
#[must_use]
pub fn generated_files() -> Vec<GeneratedFile> {
    let effects = effects::all();
    for e in &effects {
        if let Err(problems) = e.validate() {
            panic!("schema is invalid:\n  {}", problems.join("\n  "));
        }
    }
    all_files(&effects)
}
