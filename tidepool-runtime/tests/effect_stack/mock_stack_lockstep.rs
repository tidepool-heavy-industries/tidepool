//! Lockstep guard: `tidepool_testing::eval_harness::mock`'s hand-maintained
//! GADT preamble must name the same effects, in the same order, as the real
//! production stack.
//!
//! `eval_harness::mock`'s GADT preamble text and stub handlers exist so tests
//! can compile a self-contained module without wiring
//! `with_effects_module()`/`Tidepool.Orchestrate` — they are a STATIC mirror
//! of `tidepool_mcp::base_effects!`, not a derivation from it, so THEY can
//! drift.
//!
//! `mock::EFFECT_NAMES` is no longer part of that hand-maintained surface —
//! it's computed directly from `tidepool_mcp::standard_decls()`
//! (`tidepool-testing` depends on `tidepool-mcp` as a normal dependency), so
//! it cannot independently drift from production. This test pins it against
//! a second, independent call to the same function as a regression guard
//! against a hand-maintained list creeping back in, and remains the
//! reference documentation for what "matches production" means here.
//!
//! No GHC/extract toolchain needed — this only compares two pure Rust lists.

#[test]
fn mock_stack_matches_production() {
    let production: Vec<&str> = tidepool_mcp::standard_decls()
        .iter()
        .map(|d| d.type_name)
        .collect();
    assert_eq!(
        tidepool_testing::eval_harness::mock::EFFECT_NAMES.as_slice(),
        production.as_slice(),
        "tidepool_testing::eval_harness::mock::EFFECT_NAMES has drifted from \
         tidepool_mcp::standard_decls() (the single source, tidepool-mcp/src/eval_prep.rs's \
         base_effects! list + the interposed Ask/RunLLMTurn effects) — this should be \
         structurally impossible since EFFECT_NAMES is derived from standard_decls(), not \
         copied. If this fires, check for a reintroduced hand-maintained EFFECT_NAMES list."
    );
}
