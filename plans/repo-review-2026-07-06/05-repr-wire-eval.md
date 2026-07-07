# 05 — Wire format (tidepool-repr) + eval oracle robustness

Untrusted-input robustness on the production load path, plus oracle edge
cases. The wire decoder's contract is FAIL LOUD (see haskell/CLAUDE.md
one-format policy); several paths currently fail silent or not at all.

## ANTI-PATTERNS

- Do NOT change the wire arity/format (locked: CBOR via serialise/ciborium;
  metadata "array of exactly 7"). These fixes tighten VALIDATION, not format.
- F1's fix relies on both writers emitting strict post-order — verify that
  claim against `haskell/src/Tidepool/CborEncode.hs` and the Rust writer
  before enforcing `child < my_index`, and pin it with a round-trip test.

## READ FIRST

- `tidepool-repr/CLAUDE.md` (RecursiveTree scheme, DataConTable hygiene,
  wire-format versioning)
- `tidepool-repr/src/serial/read.rs` — `validate_indices` (:273-366)
- `tidepool-repr/src/tree.rs:42-81` — `extract_subtree` walk

---

## F1 (MED-HIGH): `validate_indices` accepts cyclic node graphs → first whole-tree walk hangs/OOMs on the production load path

**Where:** `serial/read.rs:273-366` + `tree.rs:42-81`. Only bounds-checks
children (`child >= len`), never the topological invariant. A corrupt payload
of ONE node `["App", 0, 0]` passes all checks; production loads it
(`tidepool-runtime/src/session/turn.rs:221`, `lib.rs:201`) and
`extract_subtree`'s memo is populated at Exit, so `Enter(0)` re-pushes itself
forever — unbounded growth, not a loud error. `varid_check.rs:99-104` already
carries a cycle guard for exactly this threat; the general walks don't.

**Fix:** check `child < my_index` in `validate_indices` (both writers emit
strict post-order — verify then enforce). Also rules out self/forward edges.
**Verify:** red test decoding the one-node cycle payload → typed
`InvalidStructure`, not a hang.

## F2 (MEDIUM, PRODUCTION PATH): freer `Union` constructor resolved by unqualified name — a user type named `Union` breaks normalize / the effect machine

**Where:** `tidepool-repr/src/normalize.rs:203`
(`get_by_name_arity("Union", 2)` returns last-inserted match); cross-crate
sibling `tidepool-effect/src/machine.rs:36` (bare `get_by_name` → `None` on
ambiguity → `MissingConstructor`).

**Failure:** if the user's `Union` has rep_arity 2, normalize can rewrite the
USER's `Con` (splicing a raw `LitWord` where codegen expects a boxed field)
and SKIP canonicalizing the real freer `Union` (only `debug_assert`ed
downstream — silent in release). `normalize` is on the production compile
path.

**Fix:** key both lookups on the QUALIFIED name, falling back to name+arity
only when no qualified entry exists. Do jointly with plan 03 F5 (hoist the
five toolchain-pinned qualified names into one place).
**Verify:** eval test with `data Union a b = Union a b` used in an effectful
program — both JIT and oracle must run it correctly.

## F3 (LOW): negative-size `newByteArray#`/`resizeMutableByteArray#` aborts the process

**Where:** `tidepool-eval/src/eval.rs:1682-1685, 1808-1816`.
`expect_int(...)? as usize` turns −1 into ~2⁶⁴; `vec![0u8; size]` aborts
(capacity overflow) — unlike the typed negative-index guards at :1857.
**Fix:** reject `size < 0` with `TypeMismatch`.

## F4 (LOW): silent truncating casts in wire decoding defeat fail-loud

**Where:** `serial/read.rs:161-162` — `tag`/`arity` via `as u32` (arity 2³²
decodes as 0); `read.rs:496` — `LitChar` codepoint truncated to u32 BEFORE the
validity check (`0x1_0000_0041` decodes as `'A'`).
**Fix:** `u32::try_from` → `InvalidStructure`/`InvalidLiteral`.

## F5 (LOW): `read_metadata` field labels decoded via `filter_map` — corrupt labels silently vanish, mis-zipping labels onto fields

**Where:** `serial/read.rs:199-207`. Make a non-Text label an error like every
other decoder in the file.

## F6 (LOW): deep `Value` spines overflow the stack when Debug/Display-formatted on error paths

**Where:** `tidepool-eval/src/value.rs:84-90` + `format!("{:?}", v)` sites
(e.g. `eval.rs:2833`). Drop/`deep_force`/`node_count` were made iterative
precisely because 50k-cons spines are normal; error formatting recurses per
Con level → SIGSEGV outside signal protection.
**Fix:** depth-capped/summary formatter for error messages.

## F7 (INFO): `parse_decimal_token` silently zeroes an unparseable JSON exponent

**Where:** `tidepool-eval/src/shapes.rs:466` (`unwrap_or(0)`): `1e9999…`
decodes as `1×10⁰` instead of erroring. No JIT divergence (shared builder),
but a silent wrong answer vs aeson. Error instead.

## Doc drift

- `datacon_table.rs:373` — comment claims `get_by_name` "would panic"; it
  returns `None`.
- `deep_force`'s `MAX_DEPTH=100_000` counts WORK-STACK length (one frame per
  pending cons), so a ~50k list already trips `DepthLimit` — the "handles long
  lists" comment oversells it. Fix comment (or count actual depth).

## Opportunities

- `child < parent` debug_asserts at the top of `extract_subtree` /
  `replace_subtree` / `free_vars` — catches malformed trees from INTERNAL
  constructors, not just the wire path (complements F1).
- `insert_checked`'s idempotent path still last-wins on tag/arity disagreement
  under an agreeing qualified name — cheap hardening.

## Verified clean — do NOT re-audit

BlackHole lifecycle (error path restores `Unevaluated`, success write-back
unconditional); SharedByteArray two-array primops clone-out before locking (no
src==dst deadlock); CBOR header/version direction correct; metadata ingestion
uses `insert_checked`; sibling-group disambiguation intact; JSON number
building shared (bridge/eval/JIT agree by construction; exact past 2^53).

## DONE CRITERIA

- [ ] F1 fixed with the cycle-payload red test; post-order claim pinned
- [ ] F2 fixed jointly with plan 03 F5; user-`Union` test green on both engines
- [ ] F3–F5 typed errors; F6 capped formatter; F7 errors
- [ ] Doc drift fixed
- [ ] `cargo nextest run -p tidepool-repr -p tidepool-eval` + differential
      lanes green
