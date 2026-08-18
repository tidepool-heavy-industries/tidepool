//! The migrated effects.
//!
//! One module per effect. An effect appears here once its whole vertical —
//! schema, every generated artifact, golden match, flip, deleted hand copy —
//! has landed; until then its definition stays in
//! `tidepool-mcp/src/effect_defs.rs`. That is PRD 22's "effect at a time,
//! smallest first, no flag day".
//!
//! Adding one: see `plans/self-iterating-harness/22-p1-protocol-scaffold.md` §9.

pub mod event;
pub mod exec;
pub mod journal;
pub mod worktree;

use crate::schema::Effect;

/// Every effect whose contract this crate owns AND generates files for.
#[must_use]
pub fn all() -> Vec<Effect> {
    vec![
        exec::exec(),
        journal::journal(),
        worktree::worktree(),
        event::event(),
    ]
}

/// Every effect described here, migrated or not — including any still awaiting
/// their flip.
///
/// Only the TESTS use this. A described-but-unflipped effect must still
/// validate, render, and pass its byte proofs; what it must not do is emit files
/// into a crate whose hand-written copy is still live. Worktree and `RepoEvent`
/// both used this mechanism (scaffold doc §11.10); both have now flipped, so
/// the two views coincide again — the seam stays for the next effect
/// described-before-flipped, and it costs one line.
#[must_use]
pub fn all_described() -> Vec<Effect> {
    all()
}
