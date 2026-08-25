//! The migrated effects.
//!
//! One module per effect. An effect appears here once its whole vertical —
//! schema, every generated artifact, golden match, flip, deleted hand copy —
//! has landed; until then its definition stays in
//! `tidepool-mcp/src/effect_defs.rs`. Effects migrate one at a time,
//! smallest first, with no flag day.

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
///
/// `AskUser`/`ReadState`/`RunLLMTurn`/`Fork`/`Finalize`/`Green` sit alongside
/// the four dispatched base effects here even though they have no
/// `tidepool-handlers` `EffectHandler` (`Effect::dispatched` is `false` for
/// all six) — `decl_rs` still owns their Haskell decl text; `crate::gen::
/// all_files` reads `dispatched` to skip `handler_rs`/`wire_rs`/`adapter_rs`
/// for them rather than emitting glue for a handler that does not exist.
/// They are ALSO listed in [`suspension_roster`] (a different generator,
/// `harness_req_rs`, needs their decode-only request enum) — being in both
/// lists is expected, not a duplication: two generators reading the same
/// effect data for two disjoint purposes.
#[must_use]
pub fn all() -> Vec<Effect> {
    vec![
        exec::exec(),
        journal::journal(),
        worktree::worktree(),
        event::event(),
        ask_user::ask_user(),
        read_state::read_state(),
        run_llm_turn::run_llm_turn(),
        fork::fork(),
        finalize::finalize(),
        green::green(),
    ]
}

/// Every effect described here, migrated or not — including any still awaiting
/// their flip.
///
/// Only the TESTS use this. A described-but-unflipped effect must still
/// validate, render, and pass its byte proofs; what it must not do is emit files
/// into a crate whose hand-written copy is still live. Worktree and `RepoEvent`
/// both used this mechanism; both have now flipped, so
/// the two views coincide again — the seam stays for the next effect
/// described-before-flipped, and it costs one line.
#[must_use]
pub fn all_described() -> Vec<Effect> {
    all()
}

/// The suspension-decode roster: every effect `tidepool-harness`'s
/// `classify_hole` needs constructor names + payload shapes for, generated as
/// decode-only request enums into `tidepool-harness/src/generated/`.
///
/// #20 steps 2-3: `AskUser`/`ReadState`/`RunLLMTurn`/`Fork`/`Finalize`/`Green`'s
/// Haskell decl text has fully flipped onto [`crate::gen::decl_rs`] (see
/// [`all`]'s doc) — they stay listed here too because `harness_req_rs` (this
/// generator) and `decl_rs` are disjoint GENERATORS reading the same effect
/// data for disjoint purposes, not because the effects themselves are
/// unmigrated. `Ask`'s decl text remains hand-carried in
/// `tidepool-mcp/src/effect_defs.rs`: its GADT/verb shape is fully
/// schema-described, but its surface helpers (`ask`, `isOpt`, `innerSchema`,
/// `schemaToValue`) are ordinary pure Haskell functions over the `Schema`
/// sum — not OPAQUE, not `*Sited`, wrapping no verb — so [`HelperBody`]'s
/// reviewed shapes (including the OPAQUE+Sited family that unblocked the
/// other five) cannot express them, and representing arbitrary
/// pattern-matching function bodies as schema data would be a new
/// general-purpose mechanism, not a bounded extension — see [`ask`]'s own
/// module doc and this schema's `Helper` doc ("a helper that is neither of
/// those… stays hand-written OUTSIDE the contract"). `Console`/`Subagent`
/// stay hand-carried for an unrelated reason: their macro ALSO feeds a real
/// `tidepool-handlers` `EffectHandler` projection, so flipping either would
/// need `tidepool-handlers` edits, out of this migration's scope (see each
/// module's `dispatched` doc). `Ask`/`Console`/`Subagent` do not flip through
/// [`crate::gen::decl_rs`]/[`crate::gen::wire_rs`]/[`crate::gen::handler_rs`]/
/// [`crate::gen::adapter_rs`], only through [`crate::gen::harness_req_rs`].
/// The four already-migrated outer effects (`Worktree`/`RepoEvent`/`Exec`/
/// `Journal`) are NOT repeated here — their request enums already exist,
/// generated into `tidepool-handlers`, and `tidepool-harness` (a dependent of
/// that crate already) reuses them directly rather than duplicating a second
/// generated copy.
///
/// [`HelperBody`]: crate::schema::HelperBody
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
