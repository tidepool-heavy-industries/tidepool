# W8 render-json — proptest findings

Property-fuzzing of `value_to_json` (`tidepool-runtime/src/render.rs`), the
surface every MCP eval result crosses on its way out to JSON. Render runs
**in-process** in the MCP server on the safe-Rust side (no JIT signal handler),
so a panic here is a real availability bug, not a contained crash.

Test file: `tidepool-runtime/tests/proptest_render_json.rs`
Seed file: `tidepool-runtime/tests/proptest-regressions/proptest_render_json.txt`
Run: `cargo test -p tidepool-runtime --test proptest_render_json`
Reproduce bugs: `cargo test -p tidepool-runtime --test proptest_render_json -- --ignored`

All adversarial values are built by **iterative** folds (cons-spines, Just-chains,
ByteArray-wrapper layers) — never recursive construction — so deep `Value`
spines are torn down by the iterative `Drop` in `tidepool-eval` without
overflowing the host thread. Property bodies run in 8 MB stack threads with
explicit `Config { cases }`.

## Live properties (green)

| # | Property | What it pins |
|---|----------|--------------|
| 1 | `prop_render_never_panics` | `catch_unwind(value_to_json)` is `Ok` for every generated value |
| 2 | `prop_render_stable` | render-twice is byte-identical |
| 3 | `prop_render_reparses` | `serde_json::from_str` succeeds on all output (parse-success per spec; not bit-exact equality — see note) |
| 4 | `prop_repr_equivalence` | the 3 string reprs of equal **non-empty** content → equal JSON |
| 5 | `prop_truncation_prefix` | below-cap list of N renders to exactly its N elements (prefix/monotonic) |

The live generator deliberately **avoids** the one shape already triaged as a
panic (out-of-bounds Text offset) so the suite stays green; that shape lives in
the `#[ignore]`d repro + the `hunt_text_offset_panic` seeder.

## Confirmed bugs

| ID | Class | Trigger (minimal `Value`) | Observed | Expected | Site |
|----|-------|---------------------------|----------|----------|------|
| **B-panic** | panic | `Text (ByteArray "") 1 0` | panic: `range start index 1 out of range for slice of length 0` | string / graceful sentinel, never panic | `render.rs:145` |
| **B1** | equal-values-diverge | empty string as `Text""` / `LitString ""` / `[Char]=[]` | `Text`/`LitString` → `""`, empty `[Char]` → `[]` | all three → `""` | `render.rs:157` |
| **B5** | truncation off-by-one | proper list of exactly `MAX_LIST_LEN` (10000) | array len **10001** with trailing `"..."` | array len 10000, no marker (nothing truncated) | `render.rs:412` |

### B-panic — Text offset exceeding backing-array length
`value_to_json`'s Text arm computes
`end = (off + len).min(borrowed.len())` then slices `borrowed[off..end]`.
`end` is clamped to `borrowed.len()`, but `off` is **not** — so whenever
`off > borrowed.len()` the slice has `start > end` and panics. Reachable for any
`Text` whose offset field exceeds its array length (e.g. a malformed/sliced
Text). Repro: `bug_bpanic_text_offset_out_of_bounds`. Regression seed shrinks to
`Con(Text, [ByteArray([]), LitInt(1), LitInt(0)])`.
Fix direction: clamp `off` too (`let off = off.min(borrowed.len())`) or guard
`off <= end` before slicing.

### B1 — empty string representation divergence
A `Text`/`LitString` of empty content renders to the JSON string `""`, but an
empty `[Char]` is indistinguishable from any empty list and hits the
`("[]", []) => json!([])` arm, rendering `[]`. The char-vs-array heuristic
(`collect_list`) can only fire on a **non-empty** cons-spine, so an empty Haskell
`String` (`[Char]`) crosses the boundary as `[]` while `Text "" `crosses as `""`.
Repro: `bug_b1_empty_string_repr_divergence`. Non-empty content is unaffected
(verified by `prop_repr_equivalence`).

### B5 — exactly-at-cap list falsely truncated
`collect_list` checks `count >= MAX_LIST_LEN` at the **top** of the loop, after
the cap-th element has already been collected and the tail is already `[]`. For a
complete 10000-element list, `count` reaches 10000 with `current` pointing at the
terminating `[]`; the next loop iteration trips the `>=` guard and appends a
spurious `"..."` sentinel, reporting a truncation that never happened. The guard
should be `>` (or the cap check should precede consuming the final tail).
Boundary: 9999 → correct (no marker), **10000 → wrong (spurious marker)**,
10001 → correct (genuine truncation). Repro: `bug_b5_list_cap_off_by_one`.

## Cap-boundary coverage (each ±1 case asserted hit)

| Cap | Sizes exercised | Counter test | Result |
|-----|-----------------|--------------|--------|
| `MAX_LIST_LEN` = 10000 | 9999 / 10000 / 10001 | `cap_boundary_list_lengths` | 9999 full ✓, 10000 spurious-marker (B5) documented, 10001 truncated ✓ |
| `MAX_DEPTH` = 1000 | 998 / 1000 / 1002 | `cap_boundary_depth` | ≤cap unwraps to inner value ✓, >cap hits `<depth limit>` sentinel ✓ |

`cap_boundary_list_lengths` and `cap_boundary_depth` each assert all three sizes
were exercised (`hit` array all-true) and that render is panic-free, stable, and
re-parseable at every boundary point.

## Other shapes exercised (no bug — robustness confirmed)

- **Improper lists** (spine ends in an `Int`, not `[]`) — rendered as an array
  with the non-nil tail appended; no panic, no infinite loop
  (`improper_and_wrong_arity_are_deterministic`).
- **Wrong-arity tag collisions** — `Con(":", [x])` (arity 1) and
  `Con(":", [a,b,c])` (arity 3) fall through to the generic-constructor arm;
  deterministic, no panic.
- **Nested `Con("ByteArray", …)` wrapper layers** inside `Text` — unwrapped by
  the loop; render to the underlying string.
- **Invalid UTF-8 / mid-codepoint slices** inside in-bounds `Text` — caught by
  `str::from_utf8` → `<Text invalid UTF-8 …>` sentinel; never panics.
- **f64 specials** (NaN / ±Inf) — `Number::from_f64` returns `None` →
  `json!(null)`; stable and re-parseable.

## Note (out of scope, not a render bug)

`serde_json`'s own float text format is not bit-exact round-trip for very large
doubles (e.g. `1.549…e47` serialises then parses back one ULP off). This is
downstream of `value_to_json` (render is deterministic — pinned by
`prop_render_stable`), so property 3 asserts parse-**success**, not
`parsed == rendered`, exactly per the workstream spec.
