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

use crate::schema::Effect;

/// Every effect whose contract this crate owns.
#[must_use]
pub fn all() -> Vec<Effect> {
    vec![exec::exec(), journal::journal()]
}
