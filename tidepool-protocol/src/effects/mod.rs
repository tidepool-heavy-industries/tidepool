//! The migrated effects.
//!
//! One module per effect. An effect appears here once its whole vertical —
//! schema, every generated artifact, golden match, flip, deleted hand copy —
//! has landed; until then its definition stays in
//! `tidepool-mcp/src/effect_defs.rs`. That is PRD 22's "effect at a time,
//! smallest first, no flag day".
//!
//! Adding one: see `plans/self-iterating-harness/22-p1-protocol-scaffold.md` §9.

pub mod exec;
pub mod journal;
pub mod worktree;

use crate::schema::Effect;

/// Every effect whose contract this crate owns.
///
/// `worktree` is DELIBERATELY absent. Its schema entry, its Haskell rendering
/// and its two new emitters all exist and are proven by test, but listing it
/// here would write generated modules into `tidepool-mcp` and
/// `tidepool-handlers` whose mod-index collides with the still-live
/// `worktree_effect_def!` macro. Lane 3 builds the CAPABILITY; the flip is a
/// separate branch, and it begins by adding one line here. See the scaffold doc
/// §11.8.
#[must_use]
pub fn all() -> Vec<Effect> {
    vec![exec::exec(), journal::journal()]
}

/// Every effect described here, migrated or not — including the ones still
/// awaiting their flip.
///
/// Only the TESTS use this. A described-but-unflipped effect must still
/// validate, render, and pass its byte proofs; what it must not do is emit files
/// into a crate whose hand-written copy is still live.
#[must_use]
pub fn all_described() -> Vec<Effect> {
    let mut out = all();
    out.push(worktree::worktree());
    out
}
