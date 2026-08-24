//! SENTINEL for the "no hand-kept pins" discipline (root `CLAUDE.md`): pins
//! the ordered SHAPE of `base_effects!`'s roster plus `standard_decls()`'s
//! interposed tail — the ONE list that, when it changes, has historically
//! broken separately-maintained satellites nobody remembered to check.
//!
//! `Entropy` joining the base stack broke three of them in one lane, each
//! found by a DIFFERENT later lane: an inline golden string in
//! `preamble.rs`'s `import_gating_pin` module, two protocol-golden lines, and
//! a hardcoded union-tag constant (`FORK_TAG`, 10 → 11). A fourth
//! (`RUN_LLM_TURN_TAG` in `run_llm_turn_sidecar.rs`, stuck at 9 when the real
//! tag had moved to 10) went undetected for a full lane because that suite
//! only runs under the GHC-heavy sharded tier — this sentinel is cheap and
//! runs in the fast default tier, so the NEXT such change fails loudly here
//! before it can hide the same way.
//!
//! This test does not (and cannot) prevent drift by itself — it is a
//! deliberately hand-maintained trip-wire: the assertion below is meant to
//! start failing the moment the roster's shape changes, and the failure
//! message IS the checklist of what to do next.

/// `base_effects!`'s own roster (`tidepool-mcp/src/eval_prep.rs`), in order.
const BASE_EFFECTS: &[&str] = &[
    "Console", "KV", "Fs", "Http", "Exec", "Llm", "Git", "Time", "Entropy",
];

/// `standard_decls()`'s interposed tail, appended after `BASE_EFFECTS`.
const INTERPOSED_TAIL: &[&str] = &["Ask", "RunLLMTurn"];

#[test]
fn effect_roster_shape_sentinel() {
    let actual: Vec<&str> = tidepool_mcp::standard_decls()
        .iter()
        .map(|d| d.type_name)
        .collect();
    let mut expected: Vec<&str> = BASE_EFFECTS.to_vec();
    expected.extend_from_slice(INTERPOSED_TAIL);

    assert_eq!(
        actual, expected,
        "\n\
         EFFECT ROSTER SHAPE CHANGED — `base_effects!`/`standard_decls()`'s \
         ordered effect list no longer matches this sentinel's pinned shape \
         (BASE_EFFECTS/INTERPOSED_TAIL above).\n\n\
         This is the deliberate checklist gate for a change that has broken \
         hand-kept pin families before (see this file's module doc). On ANY \
         effect insert/remove/reorder in `base_effects!` or `standard_decls()`'s \
         interposed tail:\n\n\
         1. Update this sentinel's BASE_EFFECTS/INTERPOSED_TAIL to the new \
            shape — that edit IS the checklist item this test enforces; do it \
            deliberately, not blindly.\n\
         2. Regenerate the protocol goldens (the ONE regen command, never a \
            second one):\n\
         \x20   TIDEPOOL_REGEN_PROTOCOL_GOLDENS=1 cargo test -p tidepool-mcp --test protocol_goldens\n\
         \x20  then review the diff under tidepool-mcp/tests/goldens/protocol/ \
            (including the import_gating.*.txt goldens) — an unexplained change \
            there is a bug, not a refresh.\n\
         3. Union-tag POSITION constants need NO manual update: \
            `fork_tag()` (tidepool-runtime/tests/jit_surface.rs) and \
            `run_llm_turn_tag()` (tidepool-runtime/tests/run_llm_turn_sidecar.rs) \
            both derive their tag from `tidepool_testing::effect_tags::tag_of` \
            against the SAME decl list, not a hand-copied integer. Confirm no \
            NEW hardcoded union-tag literal was introduced instead: \
            `rg -n 'TAG.*=.*[0-9]|tag == [0-9]|tag, [0-9]'` across the test dirs \
            should turn up nothing tied to `standard_decls()`'s ordering.\n"
    );
}
