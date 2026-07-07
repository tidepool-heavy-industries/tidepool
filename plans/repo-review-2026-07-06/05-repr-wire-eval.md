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

**STATUS: DONE.** Post-order claim verified against both writers before
enforcing: `Tidepool.CborEncode.emitNode` (`haskell/src/Tidepool/CborEncode.hs`)
calls `TransM`'s `emitNode`, which is `Seq.length (tsNodes s)` (the node's own
new index) THEN appends — so a node's index is only known/usable as a child
reference AFTER every recursive `emitNode` call building its children has
already appended them; by construction every child index is < the parent's.
Same invariant on the Rust side: `TreeBuilder::push` (`tidepool-repr/src/builder.rs`)
returns `self.nodes.len()` then pushes — every internal tree constructor in
this codebase (proptest generators in `tidepool-testing/src/gen/`, `TreeBuilder`
call sites) already respects this by construction.

`validate_indices` (`serial/read.rs`) rewritten: a shared `check_child(what,
my_idx, child)` helper replaces every per-variant `child >= len` bounds check
with `child >= my_idx` — this subsumes the old bounds check (`my_idx < len` ⇒
`child < my_idx` ⇒ `child < len`) while additionally rejecting self-loops and
forward/cyclic references. Error message carries node index + child index
context.

Red tests added (`serial/mod.rs`):
- `test_read_one_node_self_cycle_rejected` — `["App", 0, 0]` (the plan's exact
  repro) → `InvalidStructure`, not a hang.
- `test_read_forward_reference_rejected` — a 2-node array where node 0
  references node 1 (in-bounds, but not yet emitted at node 0's position).
- `test_post_order_invariant_round_trips` — encode a nontrivial tree (App,
  LetNonRec, Case with 2 alts), round-trip through `write_cbor`/`read_cbor`,
  assert every node's children have strictly smaller indices via
  `tree::get_children`.

RED observed: temporarily disabled `check_child`'s body (`if false { ... }`,
simulating the pre-fix no-topological-check state) — both cycle/forward-ref
tests failed with `Ok(...)` (the malformed tree decoded successfully instead of
erroring), confirming they exercise the fix. Restored; all `tidepool-repr`
tests green (650/650), full workspace `cargo check`/`clippy`/`fmt` clean, and
the real `-O2` Core corpus replay (`tidepool-codegen::real_core_corpus`,
`haskell_suite_differential`) still passes — the stricter check does not
reject any real fixture (as expected: real encoders already emit strict
post-order).

Opportunity (a) landed alongside this (see below): `debug_assert!(child < i)`
in `extract_subtree`/`replace_subtree` (`tree.rs`) and `free_vars` (also
covers internally-constructed trees, not just the wire path).

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

**STATUS: DONE** (normalize.rs half + single-sourcing; machine.rs half already
done in plan 03).

Single-sourcing: `freer_names` (the five toolchain-pinned constructor
names/qualified-spellings + `resolve(table, qualified, bare)`) moved from
`tidepool-effect/src/freer_names.rs` to `tidepool-repr/src/freer_names.rs` (new
module, `pub mod freer_names;` in `tidepool-repr/src/lib.rs`), all doc comments
preserved. `tidepool-effect/src/freer_names.rs` is now a one-line re-export
(`pub use tidepool_repr::freer_names::*;`), so `crate::freer_names::…` in
`machine.rs` and `tidepool_effect::freer_names::…` in `tidepool-codegen`'s
`effect_machine::ConTags::try_from` compile UNCHANGED — confirmed by full
`cargo check --workspace` (clean) and the codegen/effect test suites (below).
Dependency direction respected: `tidepool-effect` depends on `tidepool-repr`,
never the reverse, so the consts had to move DOWN into repr, matching the
spec's locked mechanism.

`normalize.rs`'s `transform_canonicalize_effect_tag`: `table.get_by_name_arity("Union",
2)` replaced with `freer_names::resolve(table, freer_names::UNION_QUALIFIED,
freer_names::UNION).filter(|id| table.get(*id).is_some_and(|dc| dc.rep_arity ==
2))` — the explicit `rep_arity == 2` filter replaces the arity-scoping
`get_by_name_arity` provided (`resolve` itself doesn't filter by arity); a
qualified match at the wrong arity now falls through to the same `None`
early-return `get_by_name_arity` would have produced. The `W#` lookup is left
as `get_by_name_arity("W#", 1)` — GHC-builtin, a different (much lower-risk)
collision class; noted here as a same-pattern follow-up, not fixed in this
pass.

Unit red test (`normalize.rs`):
`effect_tag_resolves_qualified_freer_union_not_last_inserted_user_union` — a
`DataConTable` with the real freer `Union` (qualified
`Data.OpenUnion.Union`) inserted FIRST and a user's own `data Union a b =
Union a b` (qualified `MyMod.Union`, same bare name + arity) inserted AFTER.
Asserts `table.get_by_name_arity("Union", 2)` returns the user's id (the
ambiguity the fix routes around), then normalizes a tree containing BOTH a
freer-Union `Con` (boxed `W#` tag) and a user-Union `Con` (boxed `W#` field,
NOT an effect tag), and asserts: the freer Union's tag is canonicalized to a
raw `Lit`, and the user's field stays boxed (untouched).

RED observed: reverted `transform_canonicalize_effect_tag` to
`table.get_by_name_arity("Union", 2)` — the unit test failed with "the real
freer Union's tag field must be canonicalized to a raw Lit" (the pass instead
canonicalized the user's Con, matching the finding's predicted failure mode
exactly). Restored; test green.

End-to-end test (`tidepool-runtime/tests/user_union_normalize.rs`, new file):
compiles+runs, via the JIT (`EvalHarness::run` → `compile_and_run` →
`JitEffectMachine::compile`, the only caller of `normalize()`), a program
declaring `data Union a b = Union a b`, constructing/pattern-matching it
(`unionSum (Union 3 4) == 7`) INSIDE an effectful `do` block that also performs
a real effect (`send (HttpGet "x")`) — so the real freer `Union` is genuinely
live in the same `DataConTable` as the user's. GHC ritual run from this
worktree (fresh `tidepool-extract-bin`); passes.

Oracle-side note (as invited by the STEPS if not reachable — recording here
rather than forcing it): `tidepool_eval::eval` (the tree-walking interpreter)
NEVER calls `normalize()` — confirmed by grep, the only two call sites are in
`tidepool-codegen/src/jit_machine.rs`. So this normalize.rs fix has NO
oracle-side code path to differential-test; `EvalHarness::run`/`run_pure` are
JIT-only entry points. The oracle's OWN equivalent collision
(`EffectMachine::new`'s constructor resolution) was already fixed in plan 03
via the same `freer_names::resolve` helper (machine.rs), prior to this work —
see `tidepool-runtime/tests/repro_qq_union.rs` for that regression class
(a different collision mechanism: a 64-bit varId hash collision from the `ghc`
package's transitive constructor universe, not a bare-name+arity collision).
Consequently, re-running the new e2e program's JIT compile with the F2 fix
reverted did NOT reproduce a red failure (the real extractor's insertion order
for this specific program happens to still resolve correctly via
`get_by_name_arity` — order-dependent, not guaranteed, exactly as the
deterministic unit test proves) — so the e2e test is a positive integration
confirmation ("both engines run a user-`Union` program correctly"), while the
unit test above is the actual guaranteed red/green regression guard for this
finding.

Verified: `cargo nextest run -p tidepool-repr -p tidepool-eval -p tidepool-effect
-p tidepool-optimize` (650/650), `-p tidepool-codegen` (592/592, incl.
`haskell_suite_differential` and `real_core_corpus`), the new e2e test green,
full workspace `cargo check`/`clippy`/`fmt` clean.

## F3 (LOW): negative-size `newByteArray#`/`resizeMutableByteArray#` aborts the process

**Where:** `tidepool-eval/src/eval.rs:1682-1685, 1808-1816`.
`expect_int(...)? as usize` turns −1 into ~2⁶⁴; `vec![0u8; size]` aborts
(capacity overflow) — unlike the typed negative-index guards at :1857.
**Fix:** reject `size < 0` with `TypeMismatch`.

**STATUS: DONE.** Both arms (`NewByteArray`, `ResizeMutableByteArray`) now
check `size < 0`/`new_size < 0` before casting to `usize`, returning the same
`EvalError::TypeMismatch` shape the neighboring `IndexWord8Array` negative-index
guard already used (`expected: "non-negative array index"` pattern; mirrored
here as `"non-negative array size"`).

Red tests (`eval.rs` `mod tests`): `new_byte_array_negative_size_is_typed_error`,
`resize_mutable_byte_array_negative_size_is_typed_error` — both build a
`newByteArray# (-1)` / `resizeMutableByteArray# arr (-1)` `CoreExpr` via
`TreeBuilder` and assert `EvalError::TypeMismatch`.

RED observed: reverted each guard in turn — both tests then panicked with
`thread '...' panicked at library/alloc/src/raw_vec/mod.rs:28:5: capacity
overflow` from deep inside `vec![0u8; size as usize]`/`.resize(...)` — the
allocator-level failure the finding warns about (in the JIT/production runtime,
outside a test harness's panic-catching, this class of allocator abort is not
guaranteed catchable, unlike a typed `Result` error). Restored; both tests
green.

## F4 (LOW): silent truncating casts in wire decoding defeat fail-loud

**Where:** `serial/read.rs:161-162` — `tag`/`arity` via `as u32` (arity 2³²
decodes as 0); `read.rs:496` — `LitChar` codepoint truncated to u32 BEFORE the
validity check (`0x1_0000_0041` decodes as `'A'`).
**Fix:** `u32::try_from` → `InvalidStructure`/`InvalidLiteral`.

**STATUS: DONE.** Both `tag`/`arity` in `read_metadata` and the `LitChar`
codepoint in `decode_literal` now go through `u32::try_from(...)` mapped to
`InvalidStructure`/`InvalidLiteral` respectively, erroring BEFORE
`char::from_u32`'s validity check runs (so a too-large codepoint never reaches
it in truncated form).

Red tests (`serial/read.rs` `mod tests`): `read_metadata_rejects_arity_exceeding_u32`,
`read_metadata_rejects_tag_exceeding_u32` (both build a metadata entry with
tag/arity = 2³² via raw `ciborium::value::Value`), and
`decode_lit_char_rejects_codepoint_exceeding_u32` (a `Lit(LitChar, 0x1_0000_0041)`
node).

RED observed: reverted each cast to plain `as u32` in turn —
`read_metadata_rejects_arity_exceeding_u32`/`_tag_...` both decoded successfully
with `tag`/`rep_arity` silently truncated to `0`; `decode_lit_char_...` decoded
successfully to `LitChar('A')` (`0x41`) — exactly the finding's predicted
silent-truncation failures. Restored; all three green.

## F5 (LOW): `read_metadata` field labels decoded via `filter_map` — corrupt labels silently vanish, mis-zipping labels onto fields

**Where:** `serial/read.rs:199-207`. Make a non-Text label an error like every
other decoder in the file.

**STATUS: DONE.** `filter_map` → `map` returning `Result`, collected with `?`
— consistent with every other decoder in this file (hard error, not silent
drop).

Red test: `read_metadata_rejects_non_text_field_label` — a field-labels array
`["good_label", <non-text>]`.

RED observed: reverted to `filter_map` — the test's corrupt entry decoded
successfully, silently keeping only `["good_label"]` (the non-Text label
vanished without mis-zipping in THIS single-corrupt-entry case, but the
mechanism the finding describes — a dropped label shifting the remaining
labels out of alignment with their fields — is exactly what `filter_map`
enables for a multi-label array with the corrupt entry in the middle).
Restored; test green.

## F6 (LOW): deep `Value` spines overflow the stack when Debug/Display-formatted on error paths

**Where:** `tidepool-eval/src/value.rs:84-90` + `format!("{:?}", v)` sites
(e.g. `eval.rs:2833`). Drop/`deep_force`/`node_count` were made iterative
precisely because 50k-cons spines are normal; error formatting recurses per
Con level → SIGSEGV outside signal protection.
**Fix:** depth-capped/summary formatter for error messages.

**STATUS: DONE.** Added `pub fn render_capped(v: &Value, max_depth: usize) ->
String` in `value.rs`: recurses ONLY through `Value::Con` fields, bounded by
`max_depth` (a small constant, 64 at all call sites) regardless of the value's
actual depth — so recursion depth is capped by construction, not by the input.
Past the cap, an `…` elision marker; the total node count (via the existing
iterative `Value::node_count`) is appended so the elision is informative.

Every `format!("{:?}", X)` site inside `crate::error::ValueKind::Other(...)`
construction in `eval.rs` (19 sites — grepped exhaustively for the literal
`"{:?}"` pattern, confirmed every match is a `Value`-embedding error site, none
missed) now calls `crate::value::render_capped(&X, 64)` instead.

Red tests (`value.rs` `mod tests`): `render_capped_handles_very_deep_spine_without_overflow`
builds a 200,000-deep `Con` spine (well past both `deep_force`'s 100k
`MAX_DEPTH` and any real recursion limit) and renders it via `render_capped`,
asserting completion with an elision marker and the correct total node count;
`render_capped_uncapped_for_shallow_value` pins the un-elided rendering for a
shallow value (`"<Con#1> 1 2"`).

Per the plan's explicit instruction, the crash itself is NOT pinned (a direct
`{:?}` of a 200k-deep spine is a SIGSEGV, not a catchable panic — reverting to
observe it would risk crashing the test process/runner, which the plan
correctly flags as out of scope: "do not pin the crash itself, just the capped
path"). The capped-path test alone demonstrates the fix's actual property
(bounded recursion independent of input depth): it passes for a 200k-deep
value using the SAME recursive helper structure a naive fix might use, but
`render_capped_at`'s recursion is bounded by the `max_depth` PARAMETER, never
by `v`'s structure, which is the property that matters and is directly
observable without inducing the crash.

## F7 (INFO): `parse_decimal_token` silently zeroes an unparseable JSON exponent

**Where:** `tidepool-eval/src/shapes.rs:466` (`unwrap_or(0)`): `1e9999…`
decodes as `1×10⁰` instead of erroring. No JIT divergence (shared builder),
but a silent wrong answer vs aeson. Error instead.

**STATUS: DONE, with a scope note.** `parse_decimal_token` itself is kept
non-fallible (signature `(String, i64)` unchanged) — it is called directly by
`tidepool-mcp/src/eval_prep.rs`'s `json_to_haskell` (Haskell-source-literal
rendering for the `input` binding) and, via `scientific_from_number`, by
`tidepool-bridge`'s `impl ToCore for serde_json::Value` — both OUTSIDE this
worker's ownership boundary (tidepool-mcp, tidepool-bridge), and neither has a
decode-error return path today. Changing `parse_decimal_token`'s signature
would require editing both of those out-of-scope crates.

Instead: added `pub fn decimal_token_exponent_overflows(tok: &str) -> bool` in
`shapes.rs` (detects exactly the condition `parse_decimal_token` silently
zeroes: `exp_part.parse::<i64>().is_err()`), and wired it into
`tidepool_eval::json::decode_json_str` — the actual UNTRUSTED-JSON-TEXT
decode boundary (the `JsonDecode`/`eitherDecodeValue` primop, i.e. what an
attacker-controlled program can feed at runtime — the real "eval oracle
robustness" surface this plan targets). `decode_json_str` already models
errors as `Left <err>` / `Right v` (an `Either Text Value`), so a flagged
token now short-circuits to `Left "unparseable exponent in JSON number: …"`
instead of building on it — no signature change needed there either.

This does NOT change behavior for `tidepool-bridge`'s `json_to_value`/
`scientific_from_number` call path (Rust→Haskell JSON bridging of
already-constructed `serde_json::Number`s, not raw untrusted text) or
`tidepool-mcp`'s `json_to_haskell` (`input`-binding source rendering) — both
still silently zero an out-of-range exponent, exactly as before. Flagging
this as a known follow-up that requires a cross-boundary change (touching
tidepool-mcp and/or tidepool-bridge) to close fully; out of scope for this
worker's boundary.

Red tests: `shapes.rs`'s `decimal_token_exponent_overflows_flags_unparseable_exponent`
(unit-level: flags `1e99999999999999999999`, does NOT flag `1e9223372036854775807`
== i64::MAX, ordinary tokens); `json.rs`'s
`decode_json_str_rejects_huge_exponent_as_left` (a huge-exponent JSON literal
decodes to `Left`, not `Right`) and `decode_json_str_accepts_ordinary_exponent_as_right`
(sanity counterpart).

RED observed: reverted `decode_json_str`'s overflow check to a no-op — the
huge-exponent test failed with `Con#2` (i.e. `Right (Number (Scientific 1
0))`, the `1×10⁰` the finding describes) instead of `Con#1` (`Left`). Restored;
green.

## Doc drift

- `datacon_table.rs:373` — comment claims `get_by_name` "would panic"; it
  returns `None`.
- `deep_force`'s `MAX_DEPTH=100_000` counts WORK-STACK length (one frame per
  pending cons), so a ~50k list already trips `DepthLimit` — the "handles long
  lists" comment oversells it. Fix comment (or count actual depth).

**STATUS: DONE (comments fixed, no re-count needed).**
`datacon_table.rs`'s stale comment corrected to "returns None (ambiguous)".
`deep_force`'s doc comment rewritten to state the true bound: `MAX_DEPTH`
bounds the explicit WORK STACK's length (one `Work` item per pending `Con`/
`ConFun` field), not the value's true nesting depth — a ~50k-element list
already carries that many pending fields and trips `DepthLimit`, nowhere near
what would actually threaten the (heap-allocated) host stack. Did not
re-count actual depth: no existing suite was observed tripping `DepthLimit`
in this work, so per the plan's own guidance ("only re-count actual depth if
you find the DepthLimit actually firing") the comment fix is the correct
scope — changing the counted quantity would be a behavior change, not a doc
fix.

## Opportunities

- `child < parent` debug_asserts at the top of `extract_subtree` /
  `replace_subtree` / `free_vars` — catches malformed trees from INTERNAL
  constructors, not just the wire path (complements F1).
- `insert_checked`'s idempotent path still last-wins on tag/arity disagreement
  under an agreeing qualified name — cheap hardening.

**STATUS: BOTH DONE.**

(a) `debug_assert!(c < i, ...)` added inside the `for_each_child_rev` closure
at the top of `extract_subtree`'s and `replace_subtree`'s Enter arm
(`tree.rs`), and `free_vars`'s Enter arm (`free_vars.rs`) — same invariant as
F1's wire-side `check_child`, now also covering trees built directly by
internal constructors (bypassing `read_cbor` entirely). Confirmed these don't
fire spuriously: `tidepool-repr::stack_safety`'s `extract_subtree_deep_is_stack_safe`
/ `replace_subtree_deep_is_stack_safe` / `free_vars_deep_is_stack_safe` (which
build very deep towers via the same `TreeBuilder`/proptest generators that
already respect strict post-order) all pass with the asserts active.

(b) `insert_checked` now also rejects an AGREEING qualified/unqualified
identity that DISAGREES on `tag`/`rep_arity` (previously silent last-wins via
plain `insert`), reusing the `DataConCollision` error shape with a
`"{identity} (tag=…, rep_arity=…)"` message for `first`/`second`.

Red test (`datacon_table.rs`):
`insert_checked_rejects_same_identity_disagreeing_tag_arity` — two entries
sharing id + qualified name but different tag/arity.

RED observed: disabled the tag/arity comparison (kept only the identity check)
— the test failed with "agreeing identity but disagreeing tag/arity must
collide: ()" (the second insert silently succeeded, last-wins). Restored;
green.

## Verified clean — do NOT re-audit

BlackHole lifecycle (error path restores `Unevaluated`, success write-back
unconditional); SharedByteArray two-array primops clone-out before locking (no
src==dst deadlock); CBOR header/version direction correct; metadata ingestion
uses `insert_checked`; sibling-group disambiguation intact; JSON number
building shared (bridge/eval/JIT agree by construction; exact past 2^53).

## DONE CRITERIA

- [x] F1 fixed with the cycle-payload red test; post-order claim pinned
- [x] F2 fixed jointly with plan 03 F5; user-`Union` test green on the JIT
      (the only engine that calls `normalize()` — see F2's STATUS for why
      there is no oracle-side counterpart to wire for THIS specific fix)
- [x] F3–F5 typed errors; F6 capped formatter; F7 errors (F7 scoped to the
      untrusted-JSON-text decode boundary — see F7's STATUS for the
      cross-crate follow-up noted out of scope)
- [x] Doc drift fixed
- [x] `cargo nextest run -p tidepool-repr -p tidepool-eval` + differential
      lanes green (650/650 for repr+eval+effect+optimize; 592/592 for
      codegen incl. `haskell_suite_differential`/`real_core_corpus`; 218/218
      for `haskell_suite`/`haskell_suite_differential` re-run standalone with
      a fresh worktree extract)

All boundary-owned crates (`tidepool-repr`, `tidepool-eval`, plus the
`tidepool-effect/src/freer_names.rs` single-line re-export) verified via:
`cargo check --workspace` clean, `cargo clippy --workspace --all-targets`
clean (only pre-existing, unrelated `tidepool-mcp`/`tidepool-handlers`
warnings, none touched by this work), `cargo fmt -p tidepool-repr -p
tidepool-eval -p tidepool-effect -- --check` clean (note: do NOT run `cargo
fmt --all` in this repo — it reformats files outside this worker's boundary,
namely pre-existing drift in `tidepool-repl/`, which had to be reverted
during this work; scope fmt to `-p <owned crates>` instead). Every red test
in every finding above was independently verified by temporarily reverting
its fix, observing the documented failure, and restoring — see each
finding's STATUS block for the specific observation.
