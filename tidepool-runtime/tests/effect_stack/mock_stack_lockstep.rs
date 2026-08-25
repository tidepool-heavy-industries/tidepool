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

/// Extract the effect name list from `MCP_PREAMBLE`'s `type M = Eff '[…]`
/// row — the one line in the hand-maintained preamble that names the full
/// mock stack order.
fn preamble_type_m_row() -> Vec<&'static str> {
    let preamble = tidepool_testing::eval_harness::mock::MCP_PREAMBLE;
    let line = preamble
        .lines()
        .find(|l| l.trim_start().starts_with("type M = Eff"))
        .expect("MCP_PREAMBLE must have a `type M = Eff '[…]` line");
    let inner = line
        .split("'[")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("`type M = Eff '[…]` row must be bracketed");
    inner.split(',').map(str::trim).collect()
}

/// The `EFFECT_NAMES`/`standard_decls()` pin above only proves the DERIVED
/// list can't drift from production — it says nothing about the actually
/// hand-maintained mirror (`MCP_PREAMBLE`'s GADT + `type M` row, and
/// `min_stack()`'s handler HList), which is exactly the surface that drifted
/// when Entropy joined the base stack (230f19be) and silently under-shot the
/// JIT's suspend-tag threshold. This guard fails LOUD, naming all three
/// sites, the moment that hand-maintained mirror stops matching
/// `standard_decls()` (plus the one known, intentional `Fork`-tail
/// divergence documented on `eval_harness::mock`'s module doc).
#[test]
fn preamble_and_min_stack_match_standard_decls_plus_fork_tail() {
    let production: Vec<&str> = tidepool_mcp::standard_decls()
        .iter()
        .map(|d| d.type_name)
        .collect();

    // Known, intentional divergence (see `eval_harness::mock`'s module doc):
    // the hand-maintained preamble/HList declare a trailing `Fork` that
    // `standard_decls()` does not carry.
    let mut expected = production.clone();
    expected.push("Fork");

    let preamble_row = preamble_type_m_row();
    assert_eq!(
        preamble_row, expected,
        "tidepool_testing::eval_harness::mock::MCP_PREAMBLE's `type M = Eff '[…]` row \
         ({preamble_row:?}) has drifted from tidepool_mcp::standard_decls() + the known \
         Fork-tail exception ({expected:?}). Fix in lockstep, all three sites: \
         (1) MCP_PREAMBLE's GADT declarations + `type M` row, \
         (2) mock::min_stack()'s handler HList (same order), \
         (3) tidepool_mcp::standard_decls() / tidepool-mcp/src/eval_prep.rs's base_effects! \
         list, if the drift is on the production side instead."
    );

    let min_stack_len = tidepool_testing::eval_harness::mock::min_stack().len();
    assert_eq!(
        min_stack_len,
        expected.len(),
        "tidepool_testing::eval_harness::mock::min_stack()'s handler HList has {min_stack_len} \
         entries, expected {} (tidepool_mcp::standard_decls() + the Fork-tail exception). Fix \
         in lockstep, all three sites: (1) MCP_PREAMBLE's GADT declarations + `type M` row, \
         (2) mock::min_stack()'s handler HList (same order), \
         (3) tidepool_mcp::standard_decls() / tidepool-mcp/src/eval_prep.rs's base_effects! \
         list, if the drift is on the production side instead.",
        expected.len()
    );
}
