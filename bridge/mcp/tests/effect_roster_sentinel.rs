//! SENTINEL for the "no hand-kept pins" discipline (root `CLAUDE.md`): pins
//! the ordered SHAPE of `base_effects!`'s roster plus `standard_decls()`'s
//! interposed tail — the ONE list that, when it changes, has historically
//! broken separately-maintained satellites nobody remembered to check.
//!
//! A changed effect ordering must update generated goldens and consumers of
//! union tags together. This deliberately explicit expected roster catches
//! changes before the compiler-backed suites run.

/// `base_effects!`'s own roster (`bridge/mcp/src/eval_prep.rs`), in order.
const BASE_EFFECTS: &[&str] = &[
    "Console", "KV", "FsRead", "FsWrite", "Http", "Exec", "Llm", "Git", "Time", "Entropy",
];

/// `standard_decls()`'s interposed tail, appended after `BASE_EFFECTS`.
const INTERPOSED_TAIL: &[&str] = &["Ask"];

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
         \x20  then review the diff under bridge/mcp/tests/goldens/protocol/ \
            (including the import_gating.*.txt goldens) — an unexplained change \
            there is a bug, not a refresh.\n\
         3. Confirm no hardcoded union-tag literal was introduced: \
            `rg -n 'TAG.*=.*[0-9]|tag == [0-9]|tag, [0-9]'` across the test dirs \
            should turn up nothing tied to `standard_decls()`'s ordering.\n"
    );
}

/// The substrate-marker literal exists in exactly two places by structural
/// necessity (`substrate_marker!` must be a `macro_rules!` literal because it
/// is spliced into `concat!`, which cannot take a `const` path — see the doc
/// on `tidepool_protocol::schema::SUBSTRATE_MARKER`). This converts that
/// "must stay byte-identical" note into an enforced invariant, through the
/// public surface: every substrate helper's rendered text carries the marker
/// as its first line, so any drift in `tidepool-mcp`'s copy shows up in
/// `standard_decls()` output.
#[test]
fn substrate_marker_matches_the_schema_constant() {
    let marker = tidepool_protocol::schema::SUBSTRATE_MARKER;
    let decls = tidepool_mcp::standard_decls();
    let substrate_lines: Vec<&str> = decls
        .iter()
        .flat_map(|d| d.helpers.iter().flat_map(|h| h.lines()))
        // Loose prefix on purpose: a drifted variant of the marker must be
        // CAUGHT by the equality below, not silently filtered out here.
        .filter(|line| line.trim_start().starts_with("-- @substrate"))
        .collect();
    assert!(
        !substrate_lines.is_empty(),
        "no substrate-marked helpers found in standard_decls() — either the \
         marker was renamed beyond the `-- @substrate` prefix (update this \
         test AND tidepool_protocol::schema::SUBSTRATE_MARKER together) or \
         substrate helpers vanished entirely"
    );
    for line in substrate_lines {
        assert_eq!(
            line.trim_start(),
            marker,
            "tidepool-mcp's substrate_marker! literal drifted from \
             tidepool_protocol::schema::SUBSTRATE_MARKER — the two copies \
             must stay byte-identical (see SUBSTRATE_MARKER's doc for why \
             two copies exist at all)"
        );
    }
}
