//! The one piece of session-machine test scaffolding every realm/session
//! test needs: the `C1` payload constructor. `tests/*.rs` files are separate
//! crates, so this is included via `#[path]` rather than shared as an
//! ordinary library module.
//!
//! Everything built ON TOP of `C1` lives in sibling `session_scaffold_*.rs`
//! files instead of here, each pulled in only by the tests that actually call
//! it: `session_scaffold_value.rs` (`build_value_fragment`),
//! `session_scaffold_reference.rs` (`build_reference_fragment`),
//! `session_scaffold_gc_forcing.rs` (`build_gc_forcing_fragment`),
//! `session_scaffold_expect.rs` (`expect_int`). Each `#[path]` inclusion
//! compiles as its own crate, so an item unused by THIS binary is flagged
//! `never used` even though other binaries call it — bundling every helper
//! into one file made that a near-guarantee for some subset of consumers.
//! Splitting by actual call site means no consumer ever imports a helper it
//! doesn't use.

use tidepool_repr::types::DataConId;

/// The data constructor `C1 :: Int -> T` (arity 1) shared by all fragments.
pub const C1: DataConId = DataConId(1);
