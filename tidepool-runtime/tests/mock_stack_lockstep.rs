//! Lockstep guard: `tidepool_testing::eval_harness::mock`'s hand-maintained
//! GADT preamble must name the same effects, in the same order, as the real
//! production stack.
//!
//! `eval_harness::mock` exists so tests can compile a self-contained module
//! without wiring `with_effects_module()`/`Tidepool.Orchestrate` — it is a
//! STATIC mirror of `tidepool_mcp::base_effects!`, not a derivation from it,
//! so it can drift. It already did once: the SG effect was cut in commit
//! f1a480e6 and Lsp/Time were added later, but the mock kept declaring SG and
//! Meta (never part of the base stack) long after both changes landed. This
//! test pins `mock::EFFECT_NAMES` against `tidepool_mcp::standard_decls()` so
//! a future cut/add/reorder fails loud here instead of silently going stale.
//!
//! No GHC/extract toolchain needed — this only compares two pure Rust lists.

#[test]
fn mock_stack_matches_production() {
    let production: Vec<&str> = tidepool_mcp::standard_decls()
        .iter()
        .map(|d| d.type_name)
        .collect();
    assert_eq!(
        tidepool_testing::eval_harness::mock::EFFECT_NAMES,
        production.as_slice(),
        "tidepool_testing::eval_harness::mock's hand-maintained stack has drifted from \
         tidepool_mcp::standard_decls() (the single source, tidepool-mcp/src/eval_prep.rs's \
         base_effects! list + the interposed Ask effect). Update mock::EFFECT_NAMES, \
         mock::MCP_PREAMBLE's GADT decls + `type M` line, and mock's stub handlers/min_stack \
         to match."
    );
}
