//! Derives an effect's union-tag POSITION from the same decl list a test
//! compiles against, instead of hand-copying the integer.
//!
//! Locked Decision (root `CLAUDE.md`): union tags index the effect list
//! POSITIONALLY. A test that hardcodes a tag as an integer literal silently
//! drifts the moment an earlier effect is inserted/removed/reordered — three
//! separately-maintained pin families broke this way when `Entropy` joined
//! the base stack (`FORK_TAG` in `jit_surface.rs`, plus others discovered
//! only by later, unrelated lanes). `tag_of` computes the position instead of
//! recording it, so it can never go stale: the VALUE correctness is proven by
//! dispatch itself, this is just plumbing.

use tidepool_mcp::EffectDecl;

/// The union tag of the effect named `type_name` in `decls` — its index in
/// the list. Panics (with the full list of names actually present) if no
/// decl in `decls` has that `type_name`, since a test asking for a tag that
/// isn't in its own compiled stack is a test bug, not a runtime condition.
pub fn tag_of(decls: &[EffectDecl], type_name: &str) -> u64 {
    decls
        .iter()
        .position(|d| d.type_name == type_name)
        .unwrap_or_else(|| {
            let names: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
            panic!("tag_of({type_name:?}): no such effect in the given decl list: {names:?}")
        }) as u64
}
