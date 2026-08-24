//! The migrated effects.
//!
//! One module per effect. An effect appears here once its whole vertical —
//! schema, every generated artifact, golden match, flip, deleted hand copy —
//! has landed; until then its definition stays in
//! `tidepool-mcp/src/effect_defs.rs`. That is PRD 22's "effect at a time,
//! smallest first, no flag day".
//!
//! Adding one: see `plans/self-iterating-harness/22-p1-protocol-scaffold.md` §9.

pub mod ask;
pub mod ask_user;
pub mod console;
pub mod event;
pub mod exec;
pub mod finalize;
pub mod fork;
pub mod green;
pub mod journal;
pub mod read_state;
pub mod run_llm_turn;
pub mod subagent;
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

/// The suspension-decode roster: every effect `tidepool-harness`'s
/// `classify_hole` needs constructor names + payload shapes for, generated as
/// decode-only request enums into `tidepool-harness/src/generated/` (PRD 22
/// step 1 — see `plans/self-iterating-harness/22-effect-protocol-prd.md`).
///
/// Deliberately DISJOINT from [`all`]/[`all_described`]: these nine effects'
/// Haskell decls stay hand-carried in `tidepool-mcp/src/effect_defs.rs` for
/// now (steps 2-3, held behind an operator ping — see each module's doc), so
/// none of them flip through [`crate::gen::decl_rs`]/[`crate::gen::wire_rs`]/
/// [`crate::gen::handler_rs`]/[`crate::gen::adapter_rs`], only through
/// [`crate::gen::harness_req_rs`]. The four already-migrated outer effects
/// (`Worktree`/`RepoEvent`/`Exec`/`Journal`) are NOT repeated here — their
/// request enums already exist, generated into `tidepool-handlers`, and
/// `tidepool-harness` (a dependent of that crate already) reuses them
/// directly rather than duplicating a second generated copy.
#[must_use]
pub fn suspension_roster() -> Vec<Effect> {
    vec![
        run_llm_turn::run_llm_turn(),
        fork::fork(),
        finalize::finalize(),
        ask_user::ask_user(),
        read_state::read_state(),
        subagent::subagent(),
        green::green(),
        console::console(),
        ask::ask(),
    ]
}
