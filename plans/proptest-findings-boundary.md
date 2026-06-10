# W3 — Boundary-Roundtrip Property Findings

Property-based bug hunt over the Haskell↔Rust value boundary:

- **encode / decode** — `value_to_heap` (recursion-crate hylo) and
  `heap_to_value_forcing` (iterative spine walk, `RootScope` GC rooting,
  `MAX_DEPTH = 10_000`, `MAX_FIELDS = 1024`) in
  `tidepool-codegen/src/heap_bridge.rs`.
- **marshalling** — `FromCore` / `ToCore` in `tidepool-bridge/src/impls.rs`
  (`i64`/`u64`/`f64` boxing via `I#`/`W#`/`D#`; `String` with three accepted
  reprs — `Text(ba,off,len)` / `[Char]` cons-list / `LitString`;
  `get_resilient` name+arity lookup).

Test file: `tidepool-codegen/tests/proptest_boundary_roundtrip.rs`.
Run: `cargo test -p tidepool-codegen --test proptest_boundary_roundtrip`.

All `Value` construction, comparison, canonicalization, and teardown is
iterative (worklist / fold over `Vec`) — deep `Value` spines kill host threads
via recursive `Drop` (documented bug class), so nothing here recurses over a
`Value`.

---

## Active properties (GREEN)

| # | Property | Cases | What it checks |
|---|----------|-------|----------------|
| 1 | `prop_roundtrip_identity` | 500 | `heap_to_value_forcing(value_to_heap(v)) ==canon== v` via worklist equality. Weighted generator: typical trees (3), `i64`/`f64` extremes (2), the three string reprs (3), mixed nesting (1). |
| 2 | `prop_roundtrip_gc_pressure` | 200 | 4 KiB nursery + junk allocations between encode and readback; decode must reproduce the value or refuse with a clean `NurseryExhausted`. |
| 3 | `prop_string_triple_equality` | 300 | `Text(ba,off,len)` (nonzero offset), `[Char]`, boxed `[C# Char]`, `LitString` of the same content all decode to the same Rust `String`. Multi-byte UTF-8, empty string, nonzero offsets. |
| 4 | `prop_get_resilient_collision` | 200 | `DataConTable` with multiple unqualified `I#` at different arities; both `ToCore` and `FromCore` for `i64` pick the genuine arity-1 boxing con. |
| 5 | `prop_wide_con_capstraddle` / `prop_deep_chain_capstraddle` | 80 + 80 | Field counts around `MAX_FIELDS` (1023/1024/1025) and depths around `MAX_DEPTH` (9999/10000/10001), each roundtripped in a `libc::fork`ed child; a fatal signal is a B3 finding. |
| — | `reach_counters_capstraddle_coverage` | det. | Hits each cap fencepost deterministically and asserts cap-straddling cases are ≥ 5 % of relevant runs. |

The **string-repr canonicalizer** (`canonicalize` + `canon_literal`) is itself a
deliverable: it maps `ByteArray` ↔ `LitString` (same wire bytes) and collapses
NaN bit-noise to one canonical NaN, and **nothing else** — genuine content
differences (including `Text` slice content) are preserved. The three string
*reprs* are deliberately not collapsed at the `Value` level; their equivalence
is at the decoded-`String` level and is checked by Property 3.

### Coverage counters (cap-straddle)

`1023 / 1024 / 1025` fields and `9999 / 10000 / 10001` depth are each asserted
hit ≥ 1 time, and the fencepost share is asserted ≥ 5 % of relevant runs
(`reach_counters_capstraddle_coverage`). The wide-Con and deep-chain generators
are weighted (`3:3:3:2`) toward the exact fenceposts.

Findings: encode and decode are both iterative for 2-field cons (`:` cells), so
the deep-chain arm neither overflows nor trips `MAX_DEPTH` (the spine walk does
not increment depth per element). Over-cap wide cons (≥ 1025 fields) are
**cleanly rejected** by decode with `TooManyFields` — not a crash, not a finding.

---

## Confirmed bugs

| ID | Class | Component | Observed | Expected | Repro | Seed |
|----|-------|-----------|----------|----------|-------|------|
| **B2** | decode crash on malformed input (caught panic; not a fatal signal; no memory unsafety) | `tidepool-bridge/src/impls.rs:390` — `String::from_value`, `Text` arm | A `Text(ba, off, len)` whose `off`/`len` sum overflows `usize` (e.g. negative `off`: `LitInt(-1)` → `usize::MAX`) panics `attempt to add with overflow` (debug) / `slice index starts at .. but ends at ..` (release) — the bounds check `if off + len > ba.len()` adds two `usize`s unguarded. | `Err(BridgeError::TypeMismatch { expected: "valid Text slice", .. })` — same clean rejection the non-overflowing huge-`len` path already returns. | `repro_b2_text_offset_overflow_panic` (`#[ignore]`); hunting property `prop_text_offset_malformed_clean_err` (`#[ignore]`) | `tests/proptest-regressions/proptest_boundary_roundtrip.txt`, shrinks to `off = -1, len = 1` |
| **B5** | silent roundtrip non-identity / data corruption | `tidepool-codegen/src/heap_bridge.rs:402` — `value_to_heap`, `num_fields = field_ptrs.len() as u16` | A `Con` with `2^16 + k` fields (`k ≤ MAX_FIELDS`) encodes with the u16 `num_fields` header truncated, then decodes to a **smaller** `Con` with no error. Demonstrated: 65536 fields → header `num_fields = 0` → decodes to a 0-field `Con`. The decode-side `MAX_FIELDS` guard inspects only the truncated header, so it never fires. | A clean encode error (count exceeds the representable / documented limit) **or** a round-trip-identical decode. Never a silently smaller `Con`. | `repro_b5_con_field_count_u16_truncation` (`#[ignore]`) | deterministic (`n = 65536`); no random input |

### B2 detail

Real GHC `Text` never carries a negative offset, but the bridge is a **trust
boundary** that decodes raw heap objects produced by the JIT; a miscompiled or
corrupted `Text` could carry garbage `off`/`len`. The code clearly *intends* to
validate the slice (`if off + len > ba.len()`) and merely does the arithmetic
unsafely. Suggested fix: `off.checked_add(len).map_or(true, |s| s > ba.len())`
treats overflow as out-of-range and returns the existing `TypeMismatch`.

The corruption is bounded — the wrapped slice always has `start > end`, so it is
a guaranteed panic in both build profiles, never an out-of-bounds read. (Class
is therefore a caught panic, not B3/fatal-signal and not silent corruption.)

### B5 detail

The decode side caps at `MAX_FIELDS = 1024`; the encode side has **no cap** and
silently truncates. Counts in `1025..=65535` are safe — they exceed u16 only
past 65535, and decode rejects them cleanly with `TooManyFields`. The corruption
window is `65536..=66560`, whose truncated counts `0..=1024` all look in-bounds.
Suggested fix: `u16::try_from(field_ptrs.len())` on the write side, returning a
`BridgeError` (e.g. `TooManyFields`) on overflow — symmetric with the read cap.

Both are well outside the 1024-field region of primary interest, but both are
genuine boundary defects (the spec: *crashing or silently corrupting IS the
bug*), so they are seeded and reported rather than swept.

---

## Not bugs (verified clean behavior)

- **Over-`MAX_FIELDS` decode** (1025..=65535 fields): clean
  `Err(TooManyFields)`. ✔
- **Over-`MAX_DEPTH` 2-field cons chains** (≥ 10001): decode succeeds — the
  iterative spine walk does not increment `depth` per element, so `MAX_DEPTH`
  does not bite a `:`-list. No crash, roundtrip-identical. ✔
- **Empty `ByteArray` / empty `String`**: `runtime_new_byte_array(0)` returns a
  valid 8-byte (len-prefixed) allocation, not null; encodes and decodes
  cleanly. ✔
- **Huge-but-non-overflowing `Text` `len`** (`off=2, len=i64::MAX`): clean
  `Err(TypeMismatch { expected: "valid Text slice", .. })`. ✔
- **`i64`/`u64`/`f64` extremes** (`MIN`/`MAX`/`±1`/sign-bit; NaN/±0.0/±Inf/
  subnormals): bit-exact roundtrip through `value_to_heap`/`heap_to_value`. ✔
- **`get_resilient` arity disambiguation**: with same-name `I#` decoys at other
  arities, both bridge directions select the arity-1 boxing con. ✔

---

## Limitations / caveats

- **Property 2 does not exercise live-object relocation.** The test installs a
  no-op `mock_gc_trigger`, so no Cheney copy occurs; junk allocation applies
  bump-pressure and tests readback stability, but `RootScope`'s survive-the-
  copy rooting is **not** exercised here (it would need a real collector with
  a live nursery and a forcing thunk). That path is covered by the in-crate GC
  tests (`text_filter_gc` and friends). A true rooting regression would slip
  past Property 2; this is a known gap, recorded rather than papered over.
- **u16 field-count truncation** (B5) is demonstrated only at `n = 65536`; the
  full corruption window `65536..=66560` is described but not exhaustively
  fuzzed (each case allocates ~4 MB).

---

## Status

Active suite **GREEN**: 7 passed, 3 ignored (the B2/B5 repros + the B2 hunting
property). Remove an `#[ignore]` (or run `-- --include-ignored`) to reproduce a
finding. None of the findings were fixed (out of scope — `src/` is untouched).
